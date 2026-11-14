#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::resident_fixture::{ResidentTurn, run_residents};
use hya_core::CreateSession;
use hya_core::resident::ResolvedSpawnMember;
use hya_tool::{ResolvedTool, SpawnMember};

/// Test shim: wrap an installed-bundle catalog as an [`AgentCatalog`] over
/// the compiled-in built-ins.
fn to_agent_catalog(bundles: BundleCatalog) -> AgentCatalog {
    AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog")
}

/// Collect Agent fixture bundles for a catalog.
fn agent_bundles(bundles: impl IntoIterator<Item = Bundle>) -> Vec<Bundle> {
    bundles.into_iter().collect()
}

use super::*;
use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedResource,
    ResourceView, SourceFile, prepare_package,
};
use hya_core::CategoryEntry;
use hya_core::hooks::{
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use hya_plugin::messages::{
    HookName, HookRegistration, METHOD_TOOL_CALL, ToolCallParams, ToolInfo,
};
use hya_plugin::protocol::Frame;
use hya_plugin::{PluginHost, PreparedPlugin};
use hya_proto::{
    Event, FinishReason, MailEndpoint, MailKind, MemberRunStatus, MessageId, OwnerRunId,
    RosterStatus, SubagentMode, ToolName, ToolSchema, WorkflowIdentity, WorkflowRevision,
    WorkflowStagePlan,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
};
use hya_store::{BundleInstallCandidate, BundleInstallOutcome, BundleRegistry};
use hya_tool::{
    AgentDef, FormatterPlane, InlineAgent, InteractionPlane, LspPlane, MailboxPlane, Mode,
    PermissionModel, PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane,
    Tool, ToolCtx, ToolError, ToolOperation, ToolPermission, ToolRegistry, WebSearchPlane,
    handle::ArtifactPlane,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

use crate::test_env::{EnvGuard, StableEnvGuard};

struct CountingDevProvider {
    calls: Arc<AtomicUsize>,
    inner: DevProvider,
    gate: Option<Arc<ProviderGate>>,
}

struct ProviderGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl Provider for CountingDevProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if model.as_str() == "dev" {
            return self.inner.capabilities(&ModelRef::new("hya/offline"));
        }
        self.inner.capabilities(model)
    }

    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        self.inner.configured_identity_v1()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.stream(request, session, message).await
    }
}

#[test]
fn builtin_agent_catalog_retains_semantic_identity_with_first_party() {
    let _env = StableEnvGuard::new();
    let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
    assert!(
        catalog
            .semantic_identity_v1()
            .is_some_and(|identity| !identity.is_empty()),
        "first-party catalog must yield a semantic identity"
    );
}

async fn review_runtime() -> (BuiltSessionEngine, AgentSpec, SessionId) {
    let (router, model) = offline_router(None);
    let mut agent = agent_with_model(&model, None);
    agent.workdir = tempdir();
    let built = build_session_engine_pure(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let root = built
        .engine()
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
    (built, agent, root)
}

struct SessionRecordingProvider {
    sessions: Arc<std::sync::Mutex<Vec<SessionId>>>,
}

#[async_trait]
impl Provider for SessionRecordingProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        DevProvider::new().capabilities(model)
    }
    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        DevProvider::new().configured_identity_v1()
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.sessions.lock().unwrap().push(session);
        DevProvider::new().stream(request, session, message).await
    }
}

#[tokio::test]
async fn cold_workflow_control_run_never_requests_a_root_model_turn() {
    let _env = StableEnvGuard::new();
    let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let router = ProviderRouter::new().with(Arc::new(SessionRecordingProvider {
        sessions: sessions.clone(),
    }));
    let mut agent = agent_with_model("hya/offline", None);
    agent.workdir = tempdir();
    let directory = agent.workdir.join(".hya/workflows");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("cold.hya.md"), "---\nkind: Workflow\nname: cold\ndescription: Two stages on a cold root.\nnodes:\n  first:\n    agent: hya-scout\n    directive: First.\n  second:\n    agent: hya-scout\n    directive: Second.\n---\nflowchart TD\n  first --> second\n").unwrap();
    let mut built = build_session_engine_pure(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
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
    let result = built
        .workflow_control()
        .execute(
            root,
            Default::default(),
            hya_proto::WorkflowCommand::Run {
                name: Some("cold".into()),
                expected_revision: None,
                inputs: BTreeMap::new(),
                run: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        hya_proto::WorkflowCommandResult::Run { .. }
    ));
    // Drain joins the main task too, including a synthesis queued at the final handoff.
    built.shutdown().await.unwrap();
    let sessions = sessions.lock().unwrap();
    assert_eq!(
        sessions.iter().filter(|session| **session == root).count(),
        0,
        "unasked root provider requests: {sessions:?}"
    );
    assert_eq!(sessions.len(), 2, "only the two stages should run");
}

#[tokio::test]
async fn review_drain_joins_late_registration_before_killing_team() {
    let _env = StableEnvGuard::new();
    let (mut built, agent, root) = review_runtime().await;
    let engine = built.engine();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, agent.name.as_str())
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, agent.name.as_str())
        .unwrap();
    let supervisor = built.resident_supervisor();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding.clone(), agents.clone(), resources.clone()),
            None,
            None,
        )
        .await
        .unwrap();
    let stop = built.lifecycle.stop.clone();
    built.lifecycle.tasks.spawn(async move {
        // A dequeued registration must finish even when stop is signalled.
        stop.cancelled().await;
        supervisor
            .spawn_resident_parked(
                root,
                agent,
                (binding, agents, resources, None),
                "late registration".into(),
                None,
                None,
            )
            .await
            .unwrap();
    });
    built.drain(FinishCause::Shutdown).await;
    let registrations_joined = built.lifecycle.tasks.is_empty();
    built.shutdown().await.unwrap();
    assert!(
        registrations_joined,
        "stop drained residents before joining registration handlers"
    );
    assert!(
        engine.store().active_actor_ids().await.unwrap().is_empty(),
        "a registration escaped the first drain"
    );
}

#[test]
fn review_workflow_catalog_does_not_block_runtime_worker() {
    let _env = StableEnvGuard::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut built, agent, root) = review_runtime().await;
        let control = built.workflow_control();
        let (release, held) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
        });
        ready.await.unwrap();
        let (sender, receiver) = BoundWorkflowSender::with_capacity(1);
        let binding = built.engine().bind_runtime(&agent.workdir).unwrap();
        let plane = sender.for_binding(&binding).for_session(root);
        spawn_workflow_supervisor(receiver, control.clone(), built.lifecycle.stop.clone());
        let request = plane.execute(
            ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            hya_proto::WorkflowCommand::List,
            CancellationToken::new(),
        );
        tokio::pin!(request);
        let early = tokio::time::timeout(Duration::from_millis(100), &mut request).await;
        let overflow = tokio::time::timeout(
            Duration::from_millis(250),
            plane.execute(
                ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
                hya_proto::WorkflowCommand::State,
                CancellationToken::new(),
            ),
        )
        .await;
        release.send(()).unwrap();
        blocker.await.unwrap();
        let was_off_thread = early.is_err();
        if was_off_thread {
            request.await.unwrap();
        }
        built.shutdown().await.unwrap();
        assert!(
            matches!(overflow, Ok(Err(ToolError::Overloaded(_)))),
            "a request beyond the configured capacity was accepted: {overflow:?}"
        );
        assert!(
            was_off_thread,
            "catalog construction ran on the runtime worker instead of the occupied blocking pool"
        );
    });
}

#[tokio::test]
async fn review_workflow_token_taken_after_shutdown_rejects_request() {
    let _env = StableEnvGuard::new();
    let (mut built, _, root) = review_runtime().await;
    let control = built.workflow_control();
    built.shutdown().await.unwrap();
    // TaskTracker permits a token after close/wait: execute must check stop
    // AFTER acquiring that token before doing any catalog or durable work.
    let late_token = control.tasks.token();
    let error = control
        .execute(
            root,
            Default::default(),
            hya_proto::WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "WORKFLOW_RUNTIME_UNAVAILABLE");
    let list = control.list(None).await;
    let decorated = control.decorate(root, Default::default()).await;
    assert!(
        matches!(list, Err(crate::WorkflowControlError::RuntimeUnavailable)),
        "scope listing started after shutdown"
    );
    assert!(
        matches!(
            decorated,
            Err(crate::WorkflowControlError::RuntimeUnavailable)
        ),
        "decoration started after shutdown"
    );
    drop(late_token);
    assert!(built.lifecycle.tasks.is_empty());
}

#[tokio::test]
async fn built_session_engine_shutdown_drains_supervisor() {
    let _env = StableEnvGuard::new();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    assert!(
        engine
            .tool_schemas()
            .iter()
            .any(|s| s.name.as_str() == "bash")
    );
    built
        .shutdown()
        .await
        .expect("explicit shutdown must drain the spawn supervisor");
    assert!(
        built.lifecycle.tasks.is_empty(),
        "runtime-owned services survived shutdown"
    );
    // Second shutdown is idempotent (no supervisor join left).
    built
        .shutdown()
        .await
        .expect("idempotent shutdown after drain");
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_stops_plugin_and_mcp_sources_retained_by_retired_bindings() {
    let _env = StableEnvGuard::new();
    let root = tempdir();
    let plugin_pid = root.join("plugin.pid");
    let mcp_pid = root.join("mcp.pid");
    let with_pid = |mut command: Vec<String>, path: &Path| {
        command[2] = format!(
            "import os\nfrom pathlib import Path\nPath({}).write_text(str(os.getpid()))\n{}",
            serde_json::to_string(path).unwrap(),
            command[2]
        );
        command
    };
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "owned".into(),
            McpServerConfig {
                command: with_pid(mcp_fixture(), &mcp_pid),
                ..Default::default()
            },
        )]),
        vec![PluginSpec {
            id: "owned".into(),
            kind: hya_plugin::messages::PluginKindWire::Rust,
            command: with_pid(plugin_fixture("owned"), &plugin_pid),
            timeout_ms: Some(1000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: true,
        },
    )
    .await
    .unwrap();
    let engine = built.engine();
    let runtime = engine.runtime_registry();
    let binding = runtime.bind_turn(&root).unwrap();
    assert!(binding.resolve_tool("owned__plugin_ping").is_some());
    assert!(binding.resolve_tool("mcp__owned__ping").is_some());
    runtime
        .refresh(|candidate| {
            candidate.remove_sources(&std::collections::BTreeSet::from([
                hya_core::RuntimeSourceId::plugin("owned"),
                hya_core::RuntimeSourceId::mcp("owned"),
            ]));
            Ok(())
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), built.shutdown())
        .await
        .unwrap()
        .unwrap();
    for path in [&plugin_pid, &mcp_pid] {
        let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "source process {pid} survived shutdown"
        );
    }
    let mut ctx = sidecar_test_ctx(&root);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Tool, "*", Mode::Allow),
        Rule::new(Action::Mcp, "*", Mode::Allow),
    ]));
    ctx.permission = permission;
    for tool in ["owned__plugin_ping", "mcp__owned__ping"] {
        let result = binding
            .resolve_tool(tool)
            .unwrap()
            .tool
            .execute(&ctx, json!({}))
            .await;
        assert!(result.is_err(), "retained {tool} must stay closed");
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), built.shutdown())
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn built_shutdown_cancels_and_joins_deferred_mcp_preparation() {
    let _env = StableEnvGuard::new();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/sse", listener.local_addr().unwrap());
    let (entered, handshake) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut sse, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(sse.read(&mut buffer).await.unwrap() > 0);
        sse.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: endpoint\ndata: /messages\n\n").await.unwrap();
        let (mut post, _) = listener.accept().await.unwrap();
        assert!(post.read(&mut buffer).await.unwrap() > 0);
        entered.send(()).unwrap();
        // Initialization is pending: no prepared source owns this reader yet.
        assert_eq!(sse.read(&mut buffer).await.unwrap(), 0);
    });
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "gated".into(),
            McpServerConfig {
                url: Some(endpoint),
                transport: Some("sse".into()),
                ..Default::default()
            },
        )]),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: true,
            pure: true,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), handshake)
        .await
        .unwrap()
        .unwrap();
    assert!(!built.lifecycle.io_tasks.is_empty());
    tokio::time::timeout(Duration::from_secs(5), built.shutdown())
        .await
        .expect("shutdown must cancel MCP preparation and join its reader")
        .unwrap();
    assert!(built.lifecycle.tasks.is_empty());
    assert!(built.lifecycle.io_tasks.is_empty());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn built_shutdown_cancels_and_joins_started_workflow() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let workflow_dir = workdir.join(".hya/workflows");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("hold.hya.md"),
        r#"---
kind: Workflow
name: hold
description: Hold a provider until runtime shutdown.
nodes:
  hold:
    agent: hya-scout
    directive: WAIT FOR SHUTDOWN
---
flowchart TD
  hold
"#,
    )
    .unwrap();
    let gate = Arc::new(ProviderGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let router = ProviderRouter::new().with(Arc::new(CountingDevProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        inner: DevProvider::new(),
        gate: Some(gate.clone()),
    }));
    let mut agent = agent_with_model("hya/offline", None);
    agent.workdir = workdir.clone();
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::compile(hya_tool::PermissionModel::Danger, Vec::new()).unwrap(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: true,
        },
    )
    .await
    .unwrap();
    let engine = built.engine();
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let control = built.workflow_control();
    control
        .execute(
            lead,
            crate::WorkflowInvocation {
                delivery: WorkflowDelivery::Started,
                ..Default::default()
            },
            hya_proto::WorkflowCommand::Run {
                name: Some("hold".into()),
                expected_revision: None,
                inputs: BTreeMap::new(),
                run: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), built.shutdown())
        .await
        .expect("shutdown must cancel and join Started delivery")
        .unwrap();
    assert!(built.lifecycle.tasks.is_empty());
    assert!(control.active_run(lead).unwrap().is_none());
    let rejected = control
        .execute(
            lead,
            Default::default(),
            hya_proto::WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code(), "WORKFLOW_RUNTIME_UNAVAILABLE");
    assert!(built.lifecycle.tasks.is_empty());
    let state = engine.read_projection(lead).await.unwrap();
    assert_eq!(
        state.session.workflow.unwrap().run.unwrap().status,
        hya_proto::WorkflowRunStatus::Cancelled
    );
}

/// Building a new runtime owner closes every assistant turn a dead process
/// left open (`cause: interrupted`) before any turn of its own runs.
#[tokio::test]
async fn build_session_engine_closes_turns_a_crash_left_open() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();
    let message = hya_proto::MessageId::new();
    store
        .append_event(
            session,
            &Event::MessageStarted {
                session,
                message,
                role: hya_proto::Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        store,
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let projection = built.engine().read_projection(session).await.unwrap();
    let recovered = projection.session.messages.first().unwrap();
    assert_eq!(recovered.finish, Some(hya_proto::FinishReason::Cancelled));
    assert_eq!(recovered.cause, Some(FinishCause::Interrupted));
    built.shutdown().await.unwrap();
}

/// The engine carries the owner that claimed the store, so a restart
/// handoff can release that claim for its successor. A mismatched owner
/// fails the release and every successor with `RUNTIME_OWNER_BUSY`.
#[tokio::test]
async fn built_engine_can_release_the_runtime_owner_it_claimed() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        store.clone(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    built.shutdown().await.unwrap();
    store
        .release_runtime_owner(built.engine().runtime_owner())
        .expect("the engine names the claimed owner");
    store
        .claim_runtime_owner(OwnerRunId::new())
        .expect("a successor can claim after the release");
}

/// Building a new runtime owner terminalizes persisted Workflow work before
/// any supervisor can admit a Stage.
#[tokio::test]
async fn build_session_engine_interrupts_prior_workflow_owner() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();
    let run = hya_proto::WorkflowRunId::new();
    let prior_owner = OwnerRunId::new();
    store
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: WorkflowIdentity {
                    source: hya_proto::WorkflowSourceId::new("test:restart-flow"),
                    name: "restart-flow".to_string(),
                    revision: WorkflowRevision::from_bytes([1; 32]),
                },
                request_hash: "inputs".to_string(),
                owner: prior_owner,
                stages: vec![WorkflowStagePlan {
                    id: "stage".to_string(),
                    title: None,
                    agent: hya_proto::AgentName::new("general"),
                    mode: "once".to_string(),
                    level: 0,
                    worker_model: None,
                    selected_worker_model: None,
                    verifier_model: None,
                    selected_verifier_model: None,
                }],
            },
        )
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);

    let mut built = build_session_engine(
        store,
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();

    let current = built
        .engine()
        .read_projection(session)
        .await
        .unwrap()
        .session
        .workflow
        .unwrap()
        .run
        .unwrap();
    assert_eq!(current.status, hya_proto::WorkflowRunStatus::Interrupted);
    assert_eq!(current.error.as_deref(), Some("backend startup recovery"));
    built.shutdown().await.unwrap();
}

/// Reads remain responsive during a gated run; shutdown cancels and joins it.
#[tokio::test]
async fn workflow_supervisor_stop_drains_an_in_flight_run() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let workflow_dir = workdir.join(".hya/workflows");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("shutdown-flow.hya.md"),
        r#"---
kind: Workflow
name: shutdown-flow
description: Hold one governed member until lifecycle shutdown.
nodes:
  hold:
    agent: hya-scout
    directive: WAIT FOR SHUTDOWN
---
flowchart TD
  hold
"#,
    )
    .unwrap();

    let provider_gate = Arc::new(ProviderGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        inner: DevProvider::new(),
        gate: Some(Arc::clone(&provider_gate)),
    })));
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        builtin_agent_catalog().unwrap(),
    ));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Task,
        "*",
        Mode::Allow,
    )]));
    let (workflow_sender, workflow_rx) = BoundWorkflowSender::with_capacity(1);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::clone(&router),
            runtime,
            permission.clone(),
            EventBus::default(),
        )
        .with_workflow_sender(workflow_sender.clone()),
    );
    let base = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "build".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: base.name.clone(),
            model: base.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, base.name.as_str())
        .unwrap();
    let workflows = workflow_sender
        .for_binding(&binding)
        .for_session_with_agents(lead, agents);
    let tasks = RuntimeTaskOwner::new();
    let stop = tasks.stop.clone();
    let owner = OwnerRunId::new();
    let resident_supervisor = ResidentSupervisor::start_with_owner(Arc::clone(&engine), owner);
    let control = crate::WorkflowControl::new_with_routing(
        Arc::clone(&engine),
        base,
        resident_supervisor,
        owner,
        Arc::new(CategoryRegistry::default()),
        &tasks,
    );
    let supervisor = spawn_workflow_supervisor(workflow_rx, control, stop.clone());
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    let read_workflows = workflows.clone();
    let other = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("hya/offline"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let other_workflows = workflow_sender.for_binding(&binding).for_session(other);
    let ctx = ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows,
        permission: permission.for_session(lead),
        interaction: interaction.for_session(lead),
        spawner,
        operation: ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: MailboxPlane::disconnected(),
        lifecycle: LifecyclePlane::disconnected(),
        session: Some(lead),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        roots: vec![workdir.clone()],
        workdir,
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    };
    let run = tokio::spawn(async move {
        let workflow = hya_tool::ToolRegistry::builtins()
            .get("workflow")
            .expect("workflow tool");
        workflow
            .execute(&ctx, json!({"action": "run", "name": "shutdown-flow"}))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), provider_gate.entered.notified())
        .await
        .expect("Workflow member must reach the gated provider");
    let mut read_results = Vec::new();
    for plane in [read_workflows, other_workflows] {
        for command in [
            hya_proto::WorkflowCommand::List,
            hya_proto::WorkflowCommand::State,
        ] {
            read_results.push(
                tokio::time::timeout(
                    Duration::from_millis(250),
                    plane.execute(
                        ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
                        command,
                        CancellationToken::new(),
                    ),
                )
                .await,
            );
        }
    }
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), supervisor)
        .await
        .expect("Workflow supervisor must stop after cancelling its in-flight run")
        .expect("Workflow supervisor must not panic");
    let output = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("cancelled Workflow tool request must receive a terminal reply")
        .expect("Workflow tool task must not panic")
        .expect("Workflow cancellation is a terminal run outcome");
    tasks.shutdown().await;
    assert!(tasks.tasks.is_empty());
    for read in read_results {
        read.expect("List/State must not wait behind a running Workflow")
            .expect("Workflow read succeeds");
    }
    assert_eq!(output["kind"], "run");
    assert_eq!(output["result"]["run"]["status"], "cancelled");
}

struct RuntimeMarker(&'static str);

#[async_trait]
impl Tool for RuntimeMarker {
    fn name(&self) -> &str {
        self.0
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: format!("{} runtime marker", self.0),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

fn mcp_fixture() -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}
    else:
        result = {"content":{"ok":True},"isError":False}
    print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":result}), flush=True)
"#
        .to_string(),
    ]
}

fn plugin_fixture(id: &str) -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        format!(
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {{
            "protocol_version":1,
            "plugin":{{"id":"{id}","version":"1","kind":"rust"}},
            "hooks":[],
            "tools":[{{"name":"plugin_ping","description":"Ping","inputSchema":{{"type":"object"}}}}]
        }}
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":result}}), flush=True)
    elif req.get("method") == "shutdown":
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{}}}}), flush=True)
        sys.exit(0)
    elif "id" in req:
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{"ok":True,"output":{{}}}}}}), flush=True)
"#
        ),
    ]
}
fn tempdir() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-app-runtime-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_socket(tag: &str) -> PathBuf {
    static NEXT_SOCKET_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_SOCKET_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // A tempdir() path already fills most of macOS's 104-byte sun_path
    // budget, so unix sockets bind directly in the temp root under a
    // short unique name.
    std::env::temp_dir().join(format!("hya-sock-{tag}-{nanos}-{serial}.sock"))
}

fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

#[tokio::test]
async fn builder_uses_the_parsed_startup_snapshot() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let path = dir.join("hya/config.yaml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "reasoning: high\n").unwrap();
    let runtime = resolve_runtime(None).await;
    std::fs::write(&path, "reasoning: low\n").unwrap();
    let agent = agent_base_with_model(&runtime.model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        runtime.router,
        &agent,
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission, runtime.settings),
    )
    .await
    .unwrap();
    assert_eq!(
        built
            .engine()
            .effective_effort(&agent.model, None, None, None)
            .await
            .unwrap()
            .effort,
        Some(ReasoningEffort::High)
    );
    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn resolve_runtime_without_config_carries_but_does_not_print_the_notice() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let config_path = dir.join("hya/config.yaml");
    let _ = std::fs::remove_file(&config_path);

    let runtime = resolve_runtime(None).await;

    // Offline fallback selected: the canonical built-in hya/offline model.
    assert_eq!(runtime.model, "hya/offline");
    // The guidance is returned as DATA — resolve_runtime itself prints
    // nothing, so headless/RPC/serve callers (which never call `emit`) keep
    // a clean machine-readable stdout. Only interactive startup emits it.
    let notice = runtime
        .offline_notice
        .expect("missing-config path must carry an offline notice");
    assert!(notice.config_path.ends_with("hya/config.yaml"));
    assert!(notice.render().contains("OFFLINE"));
}

#[tokio::test]
async fn permission_only_config_is_kept_and_config_errors_fall_back_to_strict() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let config_path = dir.join("hya/config.yaml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "permission:\n  model: allow\n").unwrap();

    let runtime = resolve_runtime(None).await;
    assert_eq!(runtime.model, "hya/offline");
    assert_eq!(runtime.permission.model(), PermissionModel::Allow);
    assert!(runtime.offline_notice.is_some());
    assert_eq!(
        runtime.with_yolo(true).permission.model(),
        PermissionModel::Danger
    );

    std::fs::write(
        &config_path,
        "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: allow\n",
    )
    .unwrap();
    let fallback = resolve_runtime(None).await;
    assert_eq!(fallback.permission.model(), PermissionModel::Strict);
}

#[tokio::test]
async fn websearch_only_config_reaches_runtime() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let config_path = dir.join("hya/config.yaml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example.test/mcp\n    key: secret\n    enabled: false\n",
        )
        .unwrap();

    let runtime = resolve_runtime(None).await;

    assert_eq!(
        runtime.websearch.provider,
        hya_tool::WebSearchProvider::Parallel
    );
    assert_eq!(
        runtime.websearch.endpoint.as_deref(),
        Some("https://search.example.test/mcp")
    );
    assert_eq!(runtime.websearch.key.as_deref(), Some("secret"));
    assert!(!runtime.websearch.enabled);
    assert!(runtime.offline_notice.is_some());
}

#[tokio::test]
async fn disabled_websearch_is_not_exposed_by_engine() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        store,
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig {
                enabled: false,
                ..WebSearchConfig::default()
            },
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let _asks = built.take_asks();
    let _questions = built.take_questions();
    let _mcp = built.mcp_control();
    let _built = built;

    assert!(
        engine
            .tool_schemas()
            .iter()
            .all(|schema| schema.name.as_str() != "websearch")
    );
}

#[tokio::test]
async fn built_engine_lazily_refreshes_installed_catalog_at_root_binding() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let registry_path = home.join("hya/bundles/registry.sqlite3");
    assert!(!registry_path.exists());

    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let _asks = built.take_asks();
    let _questions = built.take_questions();
    let _mcp = built.mcp_control();
    let _built = built;
    assert!(!registry_path.exists());

    let old_binding = engine.bind_runtime(&workdir).unwrap();
    assert!(
        old_binding
            .resolve_agent("runtime-installed-agent")
            .is_none()
    );

    std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let installed = prepare_package(BundleSource::new(
        "runtime-installed",
        vec![SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: Bundle
identity:
  id: hya/runtime-installed-test
  version: 1.0.0
  publisher: hya
agents:
  - id: runtime-installed-agent
    role: main
---
You are the runtime-installed agent.
"#,
        )],
    ))
    .unwrap();
    let outcome = registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x52; 32],
                prepared_digest: installed.digest().to_string(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_020,
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

    let fresh_binding = engine.bind_root_runtime(&workdir).await.unwrap();
    assert!(
        fresh_binding
            .resolve_agent("runtime-installed-agent")
            .is_some()
    );
    assert!(
        old_binding
            .resolve_agent("runtime-installed-agent")
            .is_none()
    );
}

#[tokio::test]
async fn engine_snapshot_rejects_builder_bypass_and_publishes_deferred_set_atomically() {
    let _env = StableEnvGuard::new();
    let (router, _model) = offline_router(None);
    let builder = Arc::new(ToolRegistry::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(router),
        Arc::new(RuntimeRegistry::from_snapshot(
            builder.snapshot(),
            builtin_agent_catalog().unwrap(),
        )),
        permission,
        EventBus::default(),
    ));

    builder
        .register(Arc::new(RuntimeMarker("builder_bypass")))
        .unwrap();
    assert!(
        engine
            .tool_schemas()
            .iter()
            .all(|schema| schema.name.as_str() != "builder_bypass"),
        "mutating the retained candidate builder must not change the effective snapshot"
    );

    let saw_complete_old_view = Arc::new(AtomicBool::new(false));
    let observed = saw_complete_old_view.clone();
    let inspect_engine = engine.clone();
    let mut next = 0;
    let deferred_tools = std::iter::from_fn(move || {
        let item = match next {
            0 => Some(Arc::new(RuntimeMarker("mcp__deferred__first")) as Arc<dyn Tool>),
            1 => {
                let visible = inspect_engine
                    .tool_schemas()
                    .into_iter()
                    .map(|schema| schema.name.to_string())
                    .collect::<Vec<_>>();
                observed.store(
                    !visible.iter().any(|name| name == "mcp__deferred__first")
                        && !visible.iter().any(|name| name == "mcp__deferred__second"),
                    Ordering::SeqCst,
                );
                Some(Arc::new(RuntimeMarker("mcp__deferred__second")) as Arc<dyn Tool>)
            }
            _ => None,
        };
        next += 1;
        item
    });

    engine
        .refresh_runtime(|candidate| {
            for tool in deferred_tools {
                candidate.register_tool_with_permission(tool, ToolPermission::Mcp)?;
            }
            Ok(())
        })
        .unwrap();

    assert!(
        saw_complete_old_view.load(Ordering::SeqCst),
        "the first candidate member became visible before atomic publication"
    );
    let visible = engine
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.to_string())
        .collect::<Vec<_>>();
    assert!(visible.iter().any(|name| name == "mcp__deferred__first"));
    assert!(visible.iter().any(|name| name == "mcp__deferred__second"));
    assert!(!visible.iter().any(|name| name == "builder_bypass"));
}

#[tokio::test]
async fn engine_build_fences_residents_but_defers_queued_work_until_start() {
    let _env = StableEnvGuard::new();
    let database = tempdir().join("resident-recovery.db");
    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let queued_root = SessionId::new();
    let queued_actor = SessionId::new();
    let running_root = SessionId::new();
    let running_actor = SessionId::new();

    for (root, actor) in [(queued_root, queued_actor), (running_root, running_actor)] {
        store
            .append_event(
                root,
                &Event::SessionCreated {
                    session: root,
                    parent: None,
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: agent.workdir.to_string_lossy().into_owned(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                actor,
                &Event::SessionCreated {
                    session: actor,
                    parent: Some(root),
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: agent.workdir.to_string_lossy().into_owned(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
    }

    let queued_claim = store
        .try_claim_new(queued_root, queued_actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &queued_claim,
            queued_root,
            vec![Event::AgentRegistered {
                session: queued_root,
                agent_session: queued_actor,
                handle: "queued-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: agent.name.clone(),
                mode: SubagentMode::Resident,
            }],
        )
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &Event::ChannelCreated {
                session: queued_root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".to_string(), "main/queued-1".to_string()],
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &Event::MailSent {
                session: queued_root,
                from: "main".to_string(),
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "resume me".to_string(),
            },
        )
        .await
        .unwrap();

    let running_claim = store
        .try_claim_new(running_root, running_actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &running_claim,
            running_root,
            vec![
                Event::AgentRegistered {
                    session: running_root,
                    agent_session: running_actor,
                    handle: "running-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                },
                Event::ResidentWorkStarted {
                    session: running_root,
                    actor_session: running_actor,
                    handle: "main/running-1".to_string(),
                    epoch: running_claim.epoch,
                    inbox_through: 0,
                },
            ],
        )
        .await
        .unwrap();
    drop(store);

    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let mut built = build_session_engine(
        store.clone(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let _engine = built.engine();
    let _asks = built.take_asks();
    let _questions = built.take_questions();
    let _mcp = built.mcp_control();
    assert_eq!(built.resident_supervisor().episode_count(), 0);
    let before = store.read_projection(queued_root).await.unwrap();
    assert_eq!(before.team.roster["main/queued-1"].resident_cursor, 0);
    assert!(before.team.roster["main/queued-1"].resident_work.is_none());
    built.resident_supervisor().start_recovered().await.unwrap();
    let _built = built;

    let running = store.read_projection(running_root).await.unwrap();
    let running_entry = running.team.roster.get("main/running-1").unwrap();
    assert_eq!(running_entry.status, RosterStatus::Failed);
    assert!(running_entry.resident_work.is_none());

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let queued = store.read_projection(queued_root).await.unwrap();
            let entry = queued.team.roster.get("main/queued-1").unwrap();
            if entry.resident_cursor == 1 || entry.resident_work.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn startup_recovery_rebuilds_executable_resident_sidecar_before_queued_mail() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let registry_path = bundle_registry_path();
    std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    let stable_id = "runtime-installed-resident-agent";
    let marker_path = home.join("resident-startup-sidecar.marker");
    let marker_literal = serde_json::to_string(&marker_path.to_string_lossy().to_string()).unwrap();
    let installed = prepare_package(BundleSource::new(
        "runtime-installed-resident",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: Bundle
identity:
  id: hya/runtime-installed-resident
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agents:
  - id: runtime-installed-resident-agent
    role: subagent
    resource_view:
      allow:
        - bundle:hya/runtime-installed-resident/tool/echo
---
You are the installed resident agent.
"#,
            ),
            SourceFile::new(
                "extensions/runtime.js",
                format!(
                    r#"export default {{
  id: "runtime",
  server: async () => {{
    await Bun.write({marker_literal}, "ready");
    return {{
      tool: {{
        echo: {{
          description: "startup recovery echo",
          execute: async () => "startup-recovery-echo",
        }},
      }},
    }};
  }},
}}
"#
                ),
            ),
        ],
    ))
    .unwrap();
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let outcome = registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x53; 32],
                prepared_digest: installed.digest().to_string(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_021,
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

    let database = home.join("resident-startup-recovery.db");
    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let base = agent_with_model(&model, None);
    let root = SessionId::new();
    let actor = SessionId::new();
    store
        .append_event(
            root,
            &Event::SessionCreated {
                session: root,
                parent: None,
                agent: AgentName::new("removed-root-bundle-agent"),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            actor,
            &Event::SessionCreated {
                session: actor,
                parent: Some(root),
                agent: AgentName::new(stable_id),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();
    let claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &claim,
            root,
            vec![Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: "installed-resident-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new(stable_id),
                mode: SubagentMode::Resident,
            }],
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".to_string(), "main/installed-resident-1".to_string()],
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::MailSent {
                session: root,
                from: "main".to_string(),
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "startup recovery executable mail".to_string(),
            },
        )
        .await
        .unwrap();
    let observed_store = store.clone();

    let mut built = build_session_engine(
        store,
        router,
        &base,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .expect("startup recovery must resolve the installed resident definition");
    let engine = built.engine();
    let _asks = built.take_asks();
    let _questions = built.take_questions();
    let _mcp = built.mcp_control();
    assert_eq!(built.resident_supervisor().episode_count(), 0);
    built.resident_supervisor().start_recovered().await.unwrap();
    let _built = built;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let projection = observed_store.read_projection(root).await.unwrap();
            let Some(entry) = projection.team.roster.get("main/installed-resident-1") else {
                tokio::task::yield_now().await;
                continue;
            };
            if entry.resident_cursor >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("startup recovery must consume queued resident mail");
    assert_eq!(
        std::fs::read_to_string(&marker_path).unwrap_or_default(),
        "ready",
        "startup recovery must reconstruct and ACK executable sidecar before consuming queued mail"
    );
    let binding = engine
        .bind_runtime(&workdir)
        .expect("current binding must be available after startup recovery");
    assert_eq!(
        binding
            .resolve_agent(stable_id)
            .expect("installed resident must be in the current catalog")
            .stable_id,
        stable_id
    );
}

#[tokio::test]
async fn deferred_mcp_returns_before_slow_child_handshake() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut mcp = BTreeMap::new();
    mcp.insert(
        "slow".to_string(),
        hya_mcp::McpServerConfig {
            // Sleep longer than the assert budget so classic await-before-listen would fail.
            command: vec!["sleep".into(), "30".into()],
            ..hya_mcp::McpServerConfig::default()
        },
    );
    let started = std::time::Instant::now();
    let result = build_session_engine_with_mcp_defer(
        store,
        router,
        &agent,
        mcp,
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: true,
            pure: false,
        },
    )
    .await;
    let mut built = result.unwrap();
    let _engine = built.engine();
    let _asks = built.take_asks();
    let _questions = built.take_questions();
    let mcp_control = built.mcp_control();
    let _built = built;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "build_session_engine blocked on MCP for {elapsed:?}"
    );
    assert_eq!(
        mcp_control.status().await.get("slow"),
        Some(&hya_mcp::McpStatus::Connecting)
    );
}

#[tokio::test]
async fn hot_reload_global_plugin_publishes_hooks_in_source() {
    let _env = StableEnvGuard::new();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut command = plugin_fixture("source-hooks");
    command[2] = command[2].replace(
        "\"hooks\":[]",
        "\"hooks\":[{\"name\":\"message.user.before\"}]",
    );
    let built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        vec![PluginSpec {
            id: "source-hooks".into(),
            kind: hya_plugin::messages::PluginKindWire::Rust,
            command,
            timeout_ms: Some(1_000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let binding = built
        .engine()
        .runtime_registry()
        .bind_turn(&std::env::temp_dir())
        .unwrap();
    assert!(binding.resolve_tool("source-hooks__plugin_ping").is_some());
    assert_eq!(binding.bundle_hooks_for_agent(agent.name.as_str()).len(), 1);
}

#[tokio::test]
async fn hot_reload_deferred_mcp_does_not_retry_failed_startup_plugin() {
    let _env = StableEnvGuard::new();
    let root = std::env::temp_dir().join(format!("hya-boot-failure-{}", SessionId::new()));
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("starts");
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "ready".into(),
            McpServerConfig {
                command: mcp_fixture(),
                ..Default::default()
            },
        )]),
        vec![PluginSpec {
            id: "bad".into(),
            kind: hya_plugin::messages::PluginKindWire::Rust,
            command: vec![
                "python3".into(),
                "-c".into(),
                format!(
                    "with open({}, 'a') as f: f.write('start\\n')",
                    serde_json::to_string(&log).unwrap()
                ),
            ],
            timeout_ms: Some(1_000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: true,
            pure: false,
        },
    )
    .await
    .unwrap();
    let control = built.mcp_control();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if control.status().await.get("ready") == Some(&hya_mcp::McpStatus::Connected) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    built.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn startup_mixed_mcp_plugin_publishes_complete_sources() {
    let _env = StableEnvGuard::new();
    let mut mcp = BTreeMap::new();
    mcp.insert(
        "mixed".to_string(),
        McpServerConfig {
            command: mcp_fixture(),
            timeout_ms: Some(1_000),
            ..McpServerConfig::default()
        },
    );
    let plugins = vec![PluginSpec {
        id: "mixed-plugin".to_string(),
        kind: hya_plugin::messages::PluginKindWire::Rust,
        command: plugin_fixture("mixed-plugin"),
        timeout_ms: Some(1_000),
        env: BTreeMap::new(),
        plugin_dir: None,
        launch: Default::default(),
    }];
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let result = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        mcp,
        plugins,
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: false,
        },
    )
    .await;
    let mut built = result.unwrap();
    let engine = built.engine();
    let _ = built.take_asks();
    let _ = built.take_questions();
    let _built = built;
    let manifest = engine.runtime_registry().effective_manifest();
    // The static bundle, MCP server, and plugin each publish one source.
    assert_eq!(
        manifest.generation.get(),
        hya_proto::ConfigGeneration::INITIAL.get() + 3
    );
    assert!(manifest.sources.contains_key(&SourceId::mcp("mixed")));
    assert!(
        manifest
            .sources
            .contains_key(&SourceId::plugin("mixed-plugin"))
    );
    let names = engine
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"mcp__mixed__ping".to_string()));
    assert!(names.contains(&"mixed-plugin__plugin_ping".to_string()));
}

#[tokio::test]
async fn compat_mcp_control_publishes_and_removes_through_one_runtime_registry() {
    let _env = StableEnvGuard::new();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let _ = built.take_asks();
    let _ = built.take_questions();
    let control = built.mcp_control();
    let _built = built;
    let workdir = tempdir();
    let before = engine.runtime_registry().bind_turn(&workdir).unwrap();
    control
        .upsert(
            "dynamic".to_string(),
            McpServerConfig {
                command: mcp_fixture(),
                timeout_ms: Some(1_000),
                ..McpServerConfig::default()
            },
        )
        .await
        .unwrap();
    let connected = engine.runtime_registry().bind_turn(&workdir).unwrap();
    assert!(before.resolve_tool("mcp__dynamic__ping").is_none());
    assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
    assert_eq!(
        control.status().await.get("dynamic"),
        Some(&hya_mcp::McpStatus::Connected)
    );
    assert_eq!(
        control.tools().await.get("dynamic"),
        Some(&vec!["mcp__dynamic__ping".to_string()])
    );

    assert!(
        control
            .set_enabled("dynamic".to_string(), false)
            .await
            .unwrap()
    );
    let removed = engine.runtime_registry().bind_turn(&workdir).unwrap();
    assert!(removed.resolve_tool("mcp__dynamic__ping").is_none());
    assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
    assert_eq!(
        control.status().await.get("dynamic"),
        Some(&hya_mcp::McpStatus::Disabled)
    );
    assert_eq!(control.tools().await.get("dynamic"), None);
}

#[tokio::test]
async fn selected_model_reasoning_default_reaches_first_agent() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let config_path = dir.join("hya/config.yaml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "default_model: gateway/gpt-5.6-sol\nproviders:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: medium\n          variants: [low, medium]\n",
        )
        .unwrap();

    let runtime = resolve_runtime(None).await;

    // The model default stays in the catalog, where the per-request
    // resolver reads it; it is not baked into the root Agent, which would
    // make it outrank a stored user preference.
    assert_eq!(
        runtime.router.catalog()[0].reasoning_default,
        Some(hya_provider::ReasoningEffort::Medium)
    );
    let agent = agent_with_model(&runtime.model, None);
    assert_eq!(agent.reasoning, None);
}

#[test]
fn agent_with_model_omits_process_cwd_skill_index() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    write_skill(
        &workdir.join(".hya/skills/baseline"),
        "baseline-skill",
        "Baseline skill",
        "baseline body",
    );

    let agent = agent_with_model("fake", None);

    assert!(!agent.system_prompt.contains("Available skills"));
    assert!(
        !agent
            .system_prompt
            .contains("These skills are available on demand")
    );
    assert!(!agent.system_prompt.contains("baseline-skill"));
    assert!(!agent.system_prompt.contains("Baseline skill"));
}

/// Direct exec/RPC/goal construction still bakes Environment + AGENTS.
#[test]
fn agent_with_model_pure_keeps_environment_but_drops_agents_context() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let agents_marker = "PURE_MODE_MUST_NOT_SEE_THIS";
    std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

    let agent = agent_with_model_pure("fake", None);

    assert!(
        agent.system_prompt.contains(HARNESS_AGENT_BASE),
        "pure agent must keep the harness base: {}",
        agent.system_prompt
    );
    assert!(
        agent.system_prompt.contains("## Environment"),
        "pure agent must keep Environment: {}",
        agent.system_prompt
    );
    assert!(
        !agent.system_prompt.contains(agents_marker),
        "pure agent must not bake process-cwd AGENTS: {}",
        agent.system_prompt
    );
}

#[test]
fn with_pure_clears_mcp_marks_runtime_and_keeps_websearch() {
    let _env = StableEnvGuard::new();
    let mut runtime = offline_runtime(None, false);
    runtime.mcp.insert(
        "external".to_string(),
        hya_mcp::McpServerConfig {
            command: vec!["echo".to_string()],
            env: None,
            url: None,
            transport: None,
            enabled: None,
            timeout_ms: None,
        },
    );
    let websearch = runtime.websearch.clone();

    let runtime = runtime.with_pure(true);

    assert!(runtime.pure);
    assert!(runtime.mcp.is_empty(), "pure clears configured MCP servers");
    assert!(runtime.plugins.is_empty(), "pure clears configured plugins");
    assert_eq!(
        runtime.websearch.endpoint, websearch.endpoint,
        "websearch keeps its own configuration in pure mode"
    );
}

#[test]
fn agent_with_model_retains_environment_and_agents_context() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let agents_marker = "DIRECT_MODE_AGENTS_CONTEXT_MARKER";
    std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

    let agent = agent_with_model("fake", None);

    assert!(
        agent.system_prompt.contains(HARNESS_AGENT_BASE),
        "direct agent must keep harness base: {}",
        agent.system_prompt
    );
    assert!(
        agent.system_prompt.contains("## Environment"),
        "direct agent must bake Environment: {}",
        agent.system_prompt
    );
    assert!(
        agent.system_prompt.contains(agents_marker),
        "direct agent must bake process-cwd AGENTS: {}",
        agent.system_prompt
    );
    assert!(
        agent.system_prompt.contains("## Project context:"),
        "direct agent must use project-context separators: {}",
        agent.system_prompt
    );
}

/// Server/TUI AppState agent slot is base-only (no pre-baked AGENTS).
#[test]
fn agent_base_with_model_excludes_prebaked_agents_and_environment() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let agents_marker = "SERVER_BASE_MUST_NOT_BAKE_AGENTS";
    std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

    let agent = agent_base_with_model("fake", None);

    assert_eq!(agent.system_prompt, HARNESS_AGENT_BASE);
    assert!(
        !agent.system_prompt.contains("## Environment"),
        "server base must not bake Environment"
    );
    assert!(
        !agent.system_prompt.contains(agents_marker),
        "server base must not bake AGENTS (layered per turn)"
    );
    assert!(!agent.system_prompt.contains("## Project context:"));
}

/// `hya serve` has no working directory (ADR-0024): the server agent base
/// names none, while the client-side direct agents (exec/run/loop/goal)
/// name the caller's cwd as an absolute session workdir.
#[test]
fn server_base_has_no_workdir_and_direct_agents_name_the_caller_cwd() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);

    assert_eq!(agent_base_with_model("fake", None).workdir, PathBuf::new());
    for agent in [
        agent_with_model("fake", None),
        agent_with_model_pure("fake", None),
    ] {
        assert!(agent.workdir.is_absolute(), "{}", agent.workdir.display());
        assert_eq!(
            std::fs::canonicalize(&agent.workdir).unwrap(),
            std::fs::canonicalize(&workdir).unwrap()
        );
    }
}

/// Minimal engine whose catalog deliberately omits a recorded historical id.
async fn engine_with_catalog(catalog: Arc<AgentCatalog>) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog,
    ));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        runtime,
        permission,
        EventBus::default(),
    )
}

fn catalog_with_agents(stable_ids: &[&str]) -> Arc<AgentCatalog> {
    // One bundle per agent; built-in ids come from the compiled-in registry.
    let bundles = stable_ids
        .iter()
        .map(|stable_id| {
            Bundle {
                check: None,
                tui: None,
                format_version: 4,
                identity: BundleIdentity {
                    id: format!("hya/recovery-resolution-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                namespace: None,
                digest: format!("test-only-{stable_id}"),
                agents: vec![PreparedAgent {
                    id: AgentName::new(*stable_id),
                    description: None,
                    role: AgentRole::Main,
                    color: None,
                    prompt: Some(format!("{stable_id} recovery prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    resource_view: ResourceView::default(),
                    // Deliberately empty: recovery must not depend on can_spawn.
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                    advertise: Vec::new(),
                }],
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),

                workflow: None,
                channels: vec![],
                schemas: vec![],
                process: None,
                apis: vec![],
                permission_modes: vec![],
            }
        })
        .collect::<Vec<_>>();
    Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&bundles).expect("valid recovery catalog"),
    ))
}

#[tokio::test]
async fn recovered_resident_missing_definition_fails_closed_without_synthesis() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let engine = engine_with_catalog(catalog_with_agents(&[])).await;
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base-model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let recorded = AgentName::new("legacy-resident-only");

    let err = match engine.resolve_resident_runtime(
        engine.bind_runtime(&workdir).unwrap(),
        &base,
        &recorded,
    ) {
        Ok(_) => panic!("absent recorded id must fail typed definition missing"),
        Err(err) => err,
    };
    assert!(
        matches!(
            &err,
            CoreError::AgentDefinitionMissing { agent_id } if agent_id == "legacy-resident-only"
        ),
        "expected AgentDefinitionMissing for recorded id, got {err}"
    );
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "typed surface must remain AGENT_DEFINITION_MISSING, got {err}"
    );
    // No general/base rewrite of the recorded identity.
    assert!(
        !err.to_string().contains("`general`") && !err.to_string().contains("`build`"),
        "error must name the recorded id only, got {err}"
    );
}

#[tokio::test]
async fn recovered_resident_exact_lookup_preserves_recorded_id_without_can_spawn() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    // Agent present for exact lookup but not in anyone's can_spawn (empty lists).
    let engine = engine_with_catalog(catalog_with_agents(&["resident-helper"])).await;
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base-model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let recorded = AgentName::new("resident-helper");

    let (resolved, _runtime) = engine
        .resolve_resident_runtime(engine.bind_runtime(&workdir).unwrap(), &base, &recorded)
        .expect("exact catalog hit must resolve without can_spawn");
    assert_eq!(
        resolved.name.as_str(),
        "resident-helper",
        "recorded AgentName must remain stable"
    );
    assert_eq!(
        resolved.workdir, workdir,
        "session workdir from bind must be preserved"
    );
    assert!(
        resolved
            .system_prompt
            .contains("resident-helper recovery prompt"),
        "exact definition prompt must apply: {}",
        resolved.system_prompt
    );
    assert_ne!(
        resolved.name.as_str(),
        "general",
        "must not fall back to general"
    );
    assert_ne!(
        resolved.name.as_str(),
        "build",
        "must not rewrite to base/lead"
    );
}

#[tokio::test]
async fn recovered_resident_uses_session_workdir_not_base_workdir() {
    let _env = StableEnvGuard::new();
    let session_dir = tempdir();
    let base_dir = tempdir();
    let engine = engine_with_catalog(catalog_with_agents(&["resident-helper"])).await;
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base-model"),
        system_prompt: "lead base".to_string(),
        workdir: base_dir.clone(),
        reasoning: None,
    };
    let recorded = AgentName::new("resident-helper");

    let (resolved, _runtime) = engine
        .resolve_resident_runtime(engine.bind_runtime(&session_dir).unwrap(), &base, &recorded)
        .unwrap();
    assert_eq!(resolved.workdir, session_dir);
    assert_ne!(resolved.workdir, base_dir);
}

/// Catalog with one spawnable worker whose Bundle model_policy is explicit.
fn catalog_with_worker_policy(model_policy: ModelPolicy) -> Arc<AgentCatalog> {
    // `build` is a compiled-in built-in; only `worker` needs a bundle.
    let bundle = |stable_id: &str, role: AgentRole, can_spawn: &[&str], policy: ModelPolicy| {
        let agent = PreparedAgent {
            id: AgentName::new(stable_id),
            description: None,
            role,
            color: None,
            prompt: Some(format!("{stable_id} prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: policy,
            workdir: None,
            resource_view: ResourceView::default(),
            can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
            hook_refs: Vec::new(),
            advertise: Vec::new(),
        };
        Bundle {
            check: None,
            tui: None,
            format_version: 4,
            identity: BundleIdentity {
                id: format!("hya/spawn-model-precedence-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: format!("test-only-{stable_id}"),
            agents: vec![agent],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),

            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            permission_modes: vec![],
        }
    };
    let bundles = vec![bundle("worker", AgentRole::Subagent, &[], model_policy)];
    Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&bundles).expect("valid precedence bundle catalog"),
    ))
}

fn precedence_categories() -> CategoryRegistry {
    let mut entries = HashMap::new();
    entries.insert(
        "bundle-cat".to_string(),
        CategoryEntry::from_candidates(&["cat/bundle-model".to_string()]).unwrap(),
    );
    entries.insert(
        "inline-cat".to_string(),
        CategoryEntry::from_candidates(&["cat/inline-model".to_string()]).unwrap(),
    );
    entries.insert(
        "spawn-cat".to_string(),
        CategoryEntry::from_candidates(&["cat/spawn-model".to_string()]).unwrap(),
    );
    CategoryRegistry::from_entries(entries)
}

#[tokio::test]
async fn unconfigured_spawn_uses_remembered_model_below_explicit_override() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy::default())).await;
    engine.runtime_registry().publish_agent_model_preferences(
        [("worker".to_string(), ModelRef::new("remembered/model"))]
            .into_iter()
            .collect(),
    );
    let binding = engine
        .bind_runtime(&workdir)
        .expect("bind preference snapshot");
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base/model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let sidecar_environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        tempdir(),
    );
    let resolve = |model: Option<&str>| {
        resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &sidecar_environment,
            },
            SpawnMember {
                description: "remembered preference".to_string(),
                prompt: "resolve model only".to_string(),
                subagent_type: "worker".to_string(),
                model: model.map(str::to_string),
                ..SpawnMember::default()
            },
        )
        .expect("authorized worker must resolve")
        .agent
        .model
    };

    assert_eq!(resolve(None), ModelRef::new("remembered/model"));
    // Dispatch-time resolution: an explicit id that is not in the catalog
    // and substring-matches nothing defers to the remembered tier instead
    // of overriding verbatim. (Exact and substring dispatch are covered by
    // `resolve_dispatch_model` unit tests and the p22 e2e scenario.)
    assert_eq!(
        resolve(Some("explicit/model")),
        ModelRef::new("remembered/model")
    );
}

/// `task` effort becomes the child's `#effort`, over the Agent's resolved
/// model (here its remembered preference); a `#effort` on the `model`
/// request is honoured, an explicit `effort` beats it, and an unknown label
/// is rejected before anything spawns.
#[tokio::test]
async fn spawn_effort_becomes_the_child_model_suffix() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy::default())).await;
    engine.runtime_registry().publish_agent_model_preferences(
        [("worker".to_string(), ModelRef::new("remembered/model"))]
            .into_iter()
            .collect(),
    );
    let binding = engine.bind_runtime(&workdir).expect("bind");
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base/model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let sidecar_environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        tempdir(),
    );
    let resolve = |model: Option<&str>, effort: Option<&str>| {
        resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &sidecar_environment,
            },
            SpawnMember {
                prompt: "resolve effort".to_string(),
                subagent_type: "worker".to_string(),
                model: model.map(str::to_string),
                effort: effort.map(str::to_string),
                ..SpawnMember::default()
            },
        )
        .map(|resolved| resolved.agent.model.to_string())
    };

    assert_eq!(resolve(None, None).unwrap(), "remembered/model");
    assert_eq!(
        resolve(None, Some("high")).unwrap(),
        "remembered/model#high"
    );
    assert_eq!(
        resolve(Some("offline#low"), None).unwrap(),
        "hya/offline#low"
    );
    assert_eq!(
        resolve(Some("offline#low"), Some("high")).unwrap(),
        "hya/offline#high"
    );
    assert!(matches!(
        resolve(None, Some("ludicrous")),
        Err(SpawnError::InvalidEffort { effort, .. }) if effort == "ludicrous"
    ));
}

/// Highest-to-lowest spawn model chain, each row selecting the first set layer
/// while lower layers remain present so the winner is unambiguous.
#[tokio::test]
async fn resolve_spawn_member_model_precedence_highest_to_lowest() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let categories = precedence_categories();
    let is_servable = |_: &ModelRef| true;
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base/model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let sidecar_environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        tempdir(),
    );

    #[derive(Clone, Copy)]
    struct Case {
        label: &'static str,
        bundle_model: Option<&'static str>,
        bundle_category: Option<&'static str>,
        inline_model: Option<&'static str>,
        inline_category: Option<&'static str>,
        spawn_model: Option<&'static str>,
        spawn_category: Option<&'static str>,
        expected: &'static str,
    }

    // Cumulative lower layers prove exact order:
    // spawn model > spawn category > inline model > Bundle model >
    // inline category > Bundle category > base model.
    let cases = [
        Case {
            label: "base model",
            bundle_model: None,
            bundle_category: None,
            inline_model: None,
            inline_category: None,
            spawn_model: None,
            spawn_category: None,
            expected: "base/model",
        },
        Case {
            label: "Bundle category",
            bundle_model: None,
            bundle_category: Some("bundle-cat"),
            inline_model: None,
            inline_category: None,
            spawn_model: None,
            spawn_category: None,
            expected: "cat/bundle-model",
        },
        Case {
            label: "inline category over Bundle category",
            bundle_model: None,
            bundle_category: Some("bundle-cat"),
            inline_model: None,
            inline_category: Some("inline-cat"),
            spawn_model: None,
            spawn_category: None,
            expected: "cat/inline-model",
        },
        Case {
            label: "Bundle model over category layers",
            bundle_model: Some("bundle/model"),
            bundle_category: Some("bundle-cat"),
            inline_model: None,
            inline_category: Some("inline-cat"),
            spawn_model: None,
            spawn_category: None,
            expected: "bundle/model",
        },
        Case {
            label: "dispatchable inline model over Bundle model",
            bundle_model: Some("bundle/model"),
            bundle_category: Some("bundle-cat"),
            inline_model: Some("hya/offline"),
            inline_category: Some("inline-cat"),
            spawn_model: None,
            spawn_category: None,
            expected: "hya/offline",
        },
        Case {
            label: "spawn category over inline model",
            bundle_model: Some("bundle/model"),
            bundle_category: Some("bundle-cat"),
            inline_model: Some("inline/model"),
            inline_category: Some("inline-cat"),
            spawn_model: None,
            spawn_category: Some("spawn-cat"),
            expected: "cat/spawn-model",
        },
        Case {
            label: "dispatchable spawn model highest",
            bundle_model: Some("bundle/model"),
            bundle_category: Some("bundle-cat"),
            inline_model: Some("inline/model"),
            inline_category: Some("inline-cat"),
            spawn_model: Some("offline"),
            spawn_category: Some("spawn-cat"),
            expected: "hya/offline",
        },
    ];

    for case in cases {
        let catalog = catalog_with_worker_policy(ModelPolicy {
            model: case.bundle_model.map(str::to_string),
            category: case.bundle_category.map(str::to_string),
            reasoning: None,
        });
        let engine = engine_with_catalog(catalog).await;
        let binding = engine
            .bind_runtime(&workdir)
            .expect("bind prepared catalog");
        assert!(
            binding.resolve_agent("worker").is_some(),
            "{}: worker must resolve from prepared BundleCatalog",
            case.label
        );

        let has_inline = case.inline_model.is_some() || case.inline_category.is_some();
        let member = SpawnMember {
            description: case.label.to_string(),
            prompt: "resolve model only".to_string(),
            subagent_type: "worker".to_string(),
            model: case.spawn_model.map(str::to_string),
            category: case.spawn_category.map(str::to_string),
            effort: None,
            inline_agent: has_inline.then(|| InlineAgent {
                name: "overlay".to_string(),
                prompt: "overlay prompt".to_string(),
                model: case.inline_model.map(str::to_string),
                category: case.inline_category.map(str::to_string),
                ..InlineAgent::default()
            }),
        };

        let resolve_ctx = ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &sidecar_environment,
        };
        let resolved = resolve_spawn_member(&resolve_ctx, member)
            .unwrap_or_else(|err| panic!("{}: resolve failed: {err}", case.label));

        assert_eq!(
            resolved.agent.model.as_str(),
            case.expected,
            "{}: expected model {}",
            case.label,
            case.expected
        );
        assert_eq!(
            resolved.authorized_target.as_str(),
            "worker",
            "{}: authorized target must remain the Bundle agent",
            case.label
        );
    }
}

#[tokio::test]
async fn executable_spawn_resolution_captures_sidecar_factory_before_admission() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let engine = engine_with_catalog(Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("executable")]))
            .expect("executable fixture catalog"),
    )))
    .await;
    let binding = engine.bind_runtime(&workdir).expect("bind executable turn");
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        staging_root,
    );
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base/model"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let member = SpawnMember {
        prompt: "sidecar pre-admission".to_string(),
        subagent_type: "worker".to_string(),
        ..SpawnMember::default()
    };
    let resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        member,
    )
    .expect("authorized executable worker must resolve");
    assert!(
        resolved.sidecar_factory.is_some(),
        "executable Bundle must capture its sidecar factory before admission"
    );
}

fn activation_sidecar_fixture(hooks: &str) -> String {
    r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
            "hooks": __HOOKS__,
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "tool/call":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": {"ok": True, "output": "echo", "time_ms": 0}}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#
        .replace("__HOOKS__", hooks)
}

#[tokio::test]
async fn bundle_sidecar_ignores_unsupported_workspace_adapters() {
    let _env = StableEnvGuard::new();
    let root = tempdir();
    let staging = tempdir();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle(
            "workspace-adapter",
        )]))
        .unwrap(),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let binding = runtime.bind_turn(&root).unwrap();
    let fixture = activation_sidecar_fixture("[]").replace(
            "\"workspaceAdapters\": []",
            r#""workspaceAdapters": [{"type":"worktree","name":"unsupported","description":"ignored"}]"#,
        );
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".into(), "-c".into(), fixture],
        staging.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .unwrap()
        .unwrap();
    let mut handle = factory
        .start(SidecarStart {
            activation_id: "unsupported-adapter".into(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("unsupported workspace adapters must not break activation");
    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn bundle_sidecar_factory_exposes_declared_activation_hooks_after_ack() {
    let _env = StableEnvGuard::new();
    let mut bundle = materialized_bundle("activation-hooks");
    let event_hook_id = bundle.hooks[0].stable_id.clone();
    let before_hook = materialized_resource(
        "activation-hooks",
        "hook",
        "tool.execute.before",
        "extensions/runtime.js",
    );
    let after_hook = materialized_resource(
        "activation-hooks",
        "hook",
        "tool.execute.after",
        "extensions/runtime.js",
    );
    bundle.agents[0].hook_refs = vec![
        event_hook_id,
        before_hook.stable_id.clone(),
        after_hook.stable_id.clone(),
    ];
    bundle.hooks.extend([before_hook, after_hook]);
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("activation hook fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture activation hook turn binding");
    let staging_root = tempdir();
    let fixture = activation_sidecar_fixture(
        r#"[
                {"name": "tool.execute.before"},
                {"name": "tool.execute.after"},
                {"name": "event"}
            ]"#,
    );
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture.to_string()],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve activation hook sidecar factory")
        .expect("materialized Bundle must expose its sidecar factory");
    let mut handle = factory
        .start(SidecarStart {
            activation_id: "activation-hooks-red".to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start transient activation");
    handle
        .ready()
        .await
        .expect("activation must acknowledge ready");
    assert!(
        handle.hook_dispatcher().is_some(),
        "declared activation hooks must be exposed after ACK"
    );

    drop(handle);
    std::fs::remove_dir_all(staging_root).expect("cleanup activation staging root");
    std::fs::remove_dir_all(turn_dir).expect("cleanup activation turn directory");
}

#[tokio::test]
async fn bundle_sidecar_hook_declarations_match_captured_selected_set() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([disjoint_materialized_bundle(
            "selected-hooks",
        )]))
        .expect("selected-hooks fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture selected-hooks turn binding");
    let staging_root = tempdir();

    assert!(matches!(
        validate_bundle_sidecar_hooks(&binding, "alpha", &[]),
        Err(CoreError::Invalid(_))
    ));

    let selected_fixture = activation_sidecar_fixture(r#"[{"name": "event"}]"#);
    let selected_environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), selected_fixture],
        staging_root.clone(),
    );
    let selected_factory = selected_environment
        .factory_for(&binding, "alpha")
        .expect("resolve selected-hooks sidecar factory")
        .expect("selected alpha capabilities must expose a sidecar factory");
    let mut selected_handle = selected_factory
        .start(SidecarStart {
            activation_id: "activation-selected-hooks".to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("selected hook declaration must start");
    selected_handle
        .ready()
        .await
        .expect("selected hook declaration must acknowledge ready");
    selected_handle
        .shutdown()
        .await
        .expect("selected hook declaration must shut down cleanly");

    let extra_fixture = activation_sidecar_fixture(
        r#"[
                {"name": "event"},
                {"name": "tool.execute.before"}
            ]"#,
    );
    let extra_environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), extra_fixture],
        staging_root.clone(),
    );
    let extra_factory = extra_environment
        .factory_for(&binding, "alpha")
        .expect("resolve extra-hook sidecar factory")
        .expect("selected alpha capabilities must expose a sidecar factory");
    let extra_result = extra_factory
        .start(SidecarStart {
            activation_id: "activation-extra-hook".to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await;
    let rejected = match extra_result {
        Err(CoreError::Invalid(_)) => true,
        Ok(mut handle) => {
            let _ = handle.shutdown().await;
            false
        }
        Err(error) => panic!("unexpected extra hook declaration error: {error}"),
    };

    std::fs::remove_dir_all(&staging_root).expect("cleanup selected-hooks staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup selected-hooks turn directory");

    assert!(
        rejected,
        "unselected hook declaration must be rejected before activation"
    );
}

/// Initialization validates packaged Skills independently of an agent's view.
#[tokio::test]
async fn bundle_sidecar_skill_declarations_match_complete_package() {
    let _env = StableEnvGuard::new();
    let make_skill = |local_id: &str| {
        let content = format!(
            "---\nname: {local_id}\ndescription: {local_id} fixture\n---\n{local_id} body\n"
        );
        PreparedResource {
            local_id: local_id.to_string(),
            stable_id: format!("bundle:hya/materialized/skill/{local_id}"),
            source_path: format!("resources/skills/{local_id}.md"),
            digest: format!("{:x}", Sha256::digest(content.as_bytes())),
            content,
            binary_base64: None,
            aliases: Vec::new(),
        }
    };
    let selected = make_skill("selected");
    let unselected = make_skill("unselected");
    let mut bundle = disjoint_materialized_bundle("selected-skills");
    set_materialized_extension_content_for_path(
        &mut bundle,
        "extensions/alpha.js",
        r#"
export default {server: async () => ({
    tool: {echo: {description: "echo", execute: async () => "echo"}},
    event: async () => {},
})};
"#
        .to_string(),
    );

    bundle.agents[0]
        .resource_view
        .allow
        .push(selected.stable_id.clone());
    bundle.skills = vec![selected.clone(), unselected.clone()];

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("selected-skills fixture catalog"),
    ));
    let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let full_contributions = PluginContributionSet {
        skills: [&selected, &unselected]
            .into_iter()
            .map(|resource| hya_plugin::SkillContribution {
                id: resource.local_id.clone(),
                content: resource.content.clone(),
                digest: resource.digest.clone(),
            })
            .collect(),
        ..PluginContributionSet::default()
    };
    let source = crate::runtime_reconcile::prepared_static_bundle_source(
        "hya/materialized",
        &[selected.clone(), unselected.clone()],
        &full_contributions,
    )
    .expect("prepare selected-skills runtime source")
    .into_runtime_source();
    registry
        .refresh(|candidate| candidate.upsert_sources(vec![source]))
        .expect("publish selected-skills runtime source");
    let turn_dir = tempdir();
    let binding = registry
        .bind_turn(&turn_dir)
        .expect("capture selected-skills turn binding");

    let selected_contributions = PluginContributionSet {
        skills: vec![hya_plugin::SkillContribution {
            id: selected.local_id.clone(),
            content: selected.content.clone(),
            digest: selected.digest.clone(),
        }],
        ..PluginContributionSet::default()
    };
    assert!(validate_bundle_sidecar_skills(&binding, "alpha", &selected_contributions).is_err());
    assert!(
        validate_bundle_sidecar_skills(&binding, "alpha", &full_contributions).is_ok(),
        "complete declarations are valid even when the agent selects only one Skill"
    );
    let staging = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        plugins::bundle_sidecar_command().expect("Bun must be available"),
        staging.clone(),
    );
    let factory = environment.factory_for(&binding, "alpha").unwrap().unwrap();
    let mut handle = factory
        .start(SidecarStart {
            activation_id: "static-skills".into(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("a real activation may omit packaged Skill declarations");
    assert!(
        validate_bundle_sidecar_skills(&binding, "alpha", &PluginContributionSet::default(),)
            .is_ok(),
        "static packaged Skills do not require process declarations"
    );
    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(turn_dir).unwrap();
}

#[tokio::test]
async fn bundle_sidecar_handle_shutdown_reaps_child_and_removes_activation_staging() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("shutdown")]))
            .expect("shutdown fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture shutdown turn binding");
    let staging_root = tempdir();
    let fixture = r#"
import json, os, sys
activation_dir = os.getcwd()
sentinel = os.path.join(os.path.dirname(activation_dir), "shutdown.sentinel")
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": {}}), flush=True)
        with open(sentinel, "w", encoding="utf-8") as handle:
            handle.write("shutdown")
        break
"#;
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture.to_string()],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve shutdown sidecar factory")
        .expect("materialized Bundle must expose a sidecar factory");
    let activation_id = "activation-shutdown";
    let mut handle = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start transient shutdown activation");
    handle
        .ready()
        .await
        .expect("shutdown activation must acknowledge ready");

    let retained_tools = handle.tool_bindings();
    let ctx = sidecar_test_ctx(&turn_dir);
    handle.shutdown().await.expect("shutdown must reap child");
    assert!(
        matches!(retained_tools[0].tool.execute(&ctx, json!({})).await,
            Err(ToolError::Other(message)) if message.contains("plugin connection closed"))
    );
    assert_eq!(
        std::fs::read_to_string(staging_root.join("shutdown.sentinel"))
            .expect("shutdown fixture sentinel"),
        "shutdown"
    );
    assert!(
        !staging_root.join(activation_id).exists(),
        "activation staging must be removed after shutdown"
    );

    std::fs::remove_dir_all(staging_root).expect("cleanup shutdown staging root");
    std::fs::remove_dir_all(turn_dir).expect("cleanup shutdown turn directory");
}

#[tokio::test]
async fn bundle_sidecar_handle_drop_removes_activation_staging() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("drop-cleanup")]))
            .expect("drop cleanup fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture drop cleanup turn binding");
    let staging_root = tempdir();
    let fixture = activation_sidecar_fixture("[]");
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve drop cleanup sidecar factory")
        .expect("materialized Bundle must expose its sidecar factory");
    let activation_id = "activation-drop-cleanup";
    let activation_dir = staging_root.join(activation_id);
    let mut handle = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start transient drop cleanup activation");
    handle
        .ready()
        .await
        .expect("drop cleanup activation must acknowledge ready");
    assert!(
        activation_dir.is_dir(),
        "sidecar start must create its unique activation directory"
    );

    let tools = handle.tool_bindings();
    let tool = tools[0].tool.clone();
    let ctx = sidecar_test_ctx(&turn_dir);
    tool.execute(&ctx, json!({})).await.unwrap();
    let loss = handle.loss_token().unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(handle);
        let activation_removed = !activation_dir.exists();
        let parent_empty_or_removed = match std::fs::read_dir(&staging_root) {
            Ok(mut entries) => entries.next().is_none(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => panic!(
                "read activation staging root `{}` after handle drop: {error}",
                staging_root.display()
            ),
        };
        assert!(
            activation_removed && parent_empty_or_removed,
            "dropping BundleSidecarHandle must remove its activation directory and leave the staging parent empty"
        );
    }));

    let stopped_result = tool.execute(&ctx, json!({})).await;
    assert!(
        loss.is_cancelled(),
        "drop must close the owned connection immediately"
    );
    assert!(
        matches!(stopped_result, Err(ToolError::Other(message)) if message.contains("plugin connection closed")),
        "retained views must return Closed after handle drop"
    );

    std::fs::remove_dir_all(&staging_root).expect("cleanup drop cleanup staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup drop cleanup turn directory");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bundle_sidecar_factory_start_cancellation_removes_activation_staging() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("cancel-start")]))
            .expect("cancel-start fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture cancel-start turn binding");
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf started > sidecar-started.marker; cat >/dev/null".to_string(),
        ],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve cancel-start sidecar factory")
        .expect("materialized Bundle must expose its sidecar factory");
    let activation_id = "activation-cancel-start";
    let activation_dir = staging_root.join(activation_id);
    let marker = activation_dir.join("sidecar-started.marker");
    let start_factory = factory.clone();
    let start = tokio::spawn(async move {
        start_factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
    });

    let marker_written = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if marker.is_file() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let activation_created = activation_dir.is_dir();
    let initialize_pending = !start.is_finished();
    start.abort();
    let join_cancelled = match start.await {
        Ok(_) => false,
        Err(error) => error.is_cancelled(),
    };
    let activation_removed = !activation_dir.exists();
    let staging_empty = match std::fs::read_dir(&staging_root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    let _ = std::fs::remove_dir_all(&staging_root);
    let _ = std::fs::remove_dir_all(&turn_dir);

    assert!(
        marker_written,
        "sidecar child must write its activation marker"
    );
    assert!(
        activation_created,
        "sidecar start must create its activation directory"
    );
    assert!(
        initialize_pending,
        "sidecar initialize must remain pending before cancellation"
    );
    assert!(
        join_cancelled,
        "aborting pending factory start must cancel its task"
    );
    assert!(
        activation_removed,
        "cancelling sidecar start must remove its activation directory"
    );
    assert!(
        staging_empty,
        "cancelling sidecar start must leave staging root empty"
    );
}

#[tokio::test]
async fn bundle_sidecar_handle_exposes_transport_loss_token() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("loss-token")]))
            .expect("loss token fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture loss token turn binding");
    let staging_root = tempdir();
    let fixture = activation_sidecar_fixture("[]");
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve loss token sidecar factory")
        .expect("materialized Bundle must expose its sidecar factory");
    let activation_id = "activation-loss-token";
    let activation_dir = staging_root.join(activation_id);
    let mut handle = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start transient loss token activation");
    handle
        .ready()
        .await
        .expect("loss token activation must acknowledge ready");

    let activation_created = activation_dir.is_dir();
    let loss_token = handle.loss_token();
    handle
        .terminate()
        .await
        .expect("terminate loss token activation");
    let activation_removed = !activation_dir.exists();
    std::fs::remove_dir_all(&staging_root).expect("cleanup loss token staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup loss token turn directory");

    assert!(
        activation_created,
        "sidecar start must create its unique activation directory"
    );
    assert!(
        activation_removed,
        "explicit sidecar termination must remove activation staging"
    );
    assert!(
        loss_token.is_some(),
        "BundleSidecarHandle must expose a transport loss token"
    );
}

#[tokio::test]
async fn directory_bundle_importing_undeclared_authoring_helper_fails_before_ack() {
    let _env = StableEnvGuard::new();
    let authoring_root = tempdir();
    let staging_root = tempdir();
    let turn_dir = tempdir();
    std::fs::create_dir_all(authoring_root.join("extensions")).unwrap();
    std::fs::write(
        authoring_root.join("bundle.hya.md"),
        br#"---
kind: Bundle
identity:
  id: hya/directory-helper-import
  version: 0.0.1
  publisher: hya-tests
resources:
  tools:
    - id: echo
      path: extensions/main.js
extensions:
  js:
    - id: main
      path: extensions/main.js
agents:
  - id: directory-helper-main
    role: main
    resource_view:
      allow:
        - echo
---
You are a directory-authored executable Bundle agent.
"#,
    )
    .unwrap();
    std::fs::write(
        authoring_root.join("extensions/main.js"),
        br#"import "./helper.js";

export default {
  id: "directory-helper-import",
  server: async () => ({
    tool: {
      echo: {
        description: "echo",
        execute: async () => "ok",
      },
    },
  }),
};
"#,
    )
    .unwrap();
    std::fs::write(
        authoring_root.join("extensions/helper.js"),
        "export const helper = true;\n",
    )
    .unwrap();

    let source = BundleSource::read_directory(&authoring_root)
        .expect("read directory-authored Bundle source");
    let prepared = prepare_package(source).expect("prepare directory-authored Bundle");
    assert_eq!(prepared.bundles()[0].extensions().len(), 1);
    assert_eq!(
        prepared.bundles()[0].extensions()[0].source_path,
        "extensions/main.js"
    );
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(prepared.bundles())
            .expect("build directory-authored Bundle catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture directory-authored Bundle turn binding");
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let factory = environment
        .factory_for(&binding, "directory-helper-main")
        .expect("resolve directory-authored sidecar factory")
        .expect("selected tool must expose a sidecar factory");
    let activation_id = "directory-helper-import";
    let activation_dir = staging_root.join(activation_id);
    assert!(
        authoring_root.join("extensions/helper.js").is_file(),
        "authoring-only helper must remain present beside the source entrypoint"
    );

    let outcome = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await;
    let detail = match outcome {
        Err(CoreError::Invalid(detail)) => detail,
        Ok(mut handle) => {
            let _ = handle.shutdown().await;
            let _ = std::fs::remove_dir_all(&authoring_root);
            let _ = std::fs::remove_dir_all(&staging_root);
            let _ = std::fs::remove_dir_all(&turn_dir);
            panic!("authoring-only helper import unexpectedly reached ACK");
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&authoring_root);
            let _ = std::fs::remove_dir_all(&staging_root);
            let _ = std::fs::remove_dir_all(&turn_dir);
            panic!("unexpected sidecar startup error: {error}");
        }
    };
    assert!(
        !detail.is_empty(),
        "pre-ACK helper import failure must carry a typed diagnostic"
    );
    let authoring_helper_present = authoring_root.join("extensions/helper.js").is_file();
    let activation_removed = !activation_dir.exists();
    let staging_empty = std::fs::read_dir(&staging_root).unwrap().next().is_none();
    std::fs::remove_dir_all(&authoring_root).unwrap();
    std::fs::remove_dir_all(&staging_root).unwrap();
    std::fs::remove_dir_all(&turn_dir).unwrap();

    assert!(
        authoring_helper_present,
        "the source-tree helper must still exist when isolated activation fails"
    );
    assert!(
        activation_removed,
        "failed helper import must remove the activation staging directory"
    );
    assert!(
        staging_empty,
        "failed helper import must leave the staging root empty"
    );
}

#[tokio::test]
async fn bundle_sidecar_factory_passes_materialized_extension_to_bun() {
    let _env = StableEnvGuard::new();
    let bundle = materialized_bun_bundle("bun-extension");
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("Bun extension fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture Bun extension turn binding");
    let staging_root = tempdir();
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve Bun extension sidecar factory")
        .expect("materialized Bundle must expose a sidecar factory");
    let activation_id = "activation-bun-extension";
    let start = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await;
    let mut handle = match start {
        Ok(handle) => handle,
        Err(error) => {
            std::fs::remove_dir_all(staging_root).expect("cleanup Bun staging root");
            std::fs::remove_dir_all(turn_dir).expect("cleanup Bun turn directory");
            panic!("start Bun extension sidecar: {error}");
        }
    };
    handle
        .ready()
        .await
        .expect("Bun extension activation must acknowledge ready");
    let tool_names = handle
        .tool_bindings()
        .iter()
        .map(|binding| binding.tool.name().to_string())
        .collect::<Vec<_>>();

    handle
        .shutdown()
        .await
        .expect("shutdown Bun extension sidecar");
    assert!(
        !staging_root.join(activation_id).exists(),
        "activation staging must be removed after shutdown"
    );
    std::fs::remove_dir_all(staging_root).expect("cleanup Bun staging root");
    std::fs::remove_dir_all(turn_dir).expect("cleanup Bun turn directory");
    assert_eq!(
        tool_names,
        vec!["bundle:hya/materialized/tool/echo"],
        "Bun extension must expose exactly the materialized echo tool"
    );
}

#[tokio::test]
async fn bun_sidecars_load_only_each_agent_captured_entrypoint() {
    let _env = StableEnvGuard::new();
    let mut bundles = disjoint_materialized_bundles("bun-disjoint");
    for bundle in &mut bundles {
        set_materialized_extension_content_for_path(
            bundle,
            "extensions/alpha.js",
            r#"
export default {
  id: "alpha-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("alpha extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "alpha echo",
          execute: async () => "alpha",
        },
      },
      event: async () => {},
    }
  },
}
"#
            .to_string(),
        );
        set_materialized_extension_content_for_path(
            bundle,
            "extensions/beta.js",
            r#"
export default {
  id: "beta-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("beta extension received unexpected initialization input")
    }
    return {
      tool: {
        beta: {
          description: "beta tool",
          execute: async () => "beta",
        },
      },
      "tool.execute.before": async () => {},
    }
  },
}
"#
            .to_string(),
        );
    }

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles(bundles))
            .expect("disjoint Bun entrypoint fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&catalog));
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture disjoint Bun entrypoint turn binding");
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let alpha_factory = environment
        .factory_for(&binding, "alpha")
        .expect("resolve alpha Bun sidecar factory")
        .expect("alpha must expose a Bun sidecar factory");
    let beta_factory = environment
        .factory_for(&binding, "beta")
        .expect("resolve beta Bun sidecar factory")
        .expect("beta must expose a Bun sidecar factory");
    let alpha_activation = "bun-disjoint-alpha";
    let beta_activation = "bun-disjoint-beta";
    let mut alpha_handle = alpha_factory
        .start(SidecarStart {
            activation_id: alpha_activation.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start alpha Bun sidecar");
    alpha_handle
        .ready()
        .await
        .expect("alpha Bun sidecar must acknowledge ready");
    let mut beta_handle = beta_factory
        .start(SidecarStart {
            activation_id: beta_activation.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .expect("start beta Bun sidecar");
    beta_handle
        .ready()
        .await
        .expect("beta Bun sidecar must acknowledge ready");

    let alpha_tools = alpha_handle
        .tool_bindings()
        .iter()
        .map(|binding| binding.tool.name().to_string())
        .collect::<Vec<_>>();
    let beta_tools = beta_handle
        .tool_bindings()
        .iter()
        .map(|binding| binding.tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        alpha_tools,
        vec!["bundle:hya/materialized/tool/echo"],
        "alpha must bind only its selected tool"
    );
    assert_eq!(
        beta_tools,
        vec!["bundle:hya/materialized-beta/tool/beta"],
        "beta must bind only its selected tool"
    );
    assert!(alpha_handle.hook_dispatcher().is_some());
    assert!(beta_handle.hook_dispatcher().is_some());

    let alpha_dir = staging_root.join(alpha_activation);
    let beta_dir = staging_root.join(beta_activation);
    assert!(alpha_dir.join("extensions/alpha.js").is_file());
    assert!(!alpha_dir.join("extensions/beta.js").exists());
    assert!(beta_dir.join("extensions/beta.js").is_file());
    assert!(!beta_dir.join("extensions/alpha.js").exists());

    alpha_handle
        .shutdown()
        .await
        .expect("shutdown alpha Bun sidecar");
    beta_handle
        .shutdown()
        .await
        .expect("shutdown beta Bun sidecar");
    assert!(!alpha_dir.exists());
    assert!(!beta_dir.exists());
    assert!(
        std::fs::read_dir(&staging_root)
            .expect("read disjoint Bun staging root")
            .next()
            .is_none(),
        "disjoint sidecar shutdown must leave staging root empty"
    );

    std::fs::remove_dir_all(&staging_root).expect("cleanup disjoint Bun staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup disjoint Bun turn directory");
    assert!(!staging_root.exists());
    assert!(!turn_dir.exists());
}

fn sidecar_test_ctx(root: &Path) -> ToolCtx {
    let session = SessionId::new();
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "*",
        Mode::Allow,
    )]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: MailboxPlane::disconnected(),
        lifecycle: LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        lsp: LspPlane::default(),
        workdir: root.to_path_buf(),
        roots: vec![root.to_path_buf()],
        cancel: Default::default(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[tokio::test]
async fn one_bun_module_serves_two_agents_with_disjoint_tool_views() {
    let _env = StableEnvGuard::new();
    let root = tempdir();
    std::fs::write(
        root.join("bundle.yaml"),
        r#"kind: Bundle
identity: { id: test/shared-module, version: 1.0.0, publisher: test }
agents:
  - id: alpha
    role: subagent
    resource_view: { allow: [alpha] }
    hook_refs: [tool.execute.before]
  - id: beta
    role: subagent
    resource_view: { allow: [beta] }
    hook_refs: [tool.execute.after]
resources:
  tools:
    - { id: alpha, path: main.js }
    - { id: beta, path: main.js }
  hooks:
    - { id: tool.execute.before, path: main.js }
    - { id: tool.execute.after, path: main.js }
extensions:
  js: [{ id: main, path: main.js }]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("main.js"),
        r#"export default {
  server: async () => ({
    tool: {
      alpha: {description: "alpha", execute: async () => "alpha result"},
      beta: {description: "beta", execute: async () => "beta result"},
    },
    "tool.execute.before": async (params) => ({input: {...params.input, guarded: true}}),
    "tool.execute.after": async () => ({status: "ok", output: "after", time_ms: 0}),
  }),
};"#,
    )
    .unwrap();
    let prepared = prepare_package(BundleSource::read_directory(&root).unwrap()).unwrap();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(prepared.bundles()).unwrap(),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let binding = runtime.bind_turn(&root).unwrap();
    let staging = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        plugins::bundle_sidecar_command().expect("Bun must be available"),
        staging.clone(),
    );
    let ctx = sidecar_test_ctx(&root);
    let session = ctx.session.unwrap();
    for agent in ["alpha", "beta"] {
        let factory = environment.factory_for(&binding, agent).unwrap().unwrap();
        let mut handle = factory
            .start(SidecarStart {
                activation_id: format!("shared-{agent}"),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .unwrap();
        let tools = handle.tool_bindings();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].tool.name(),
            format!("bundle:test/shared-module/tool/{agent}")
        );
        let hooks = handle.hook_dispatcher().unwrap();
        let before = hooks
            .tool_execute_before(ToolExecuteBeforeInput {
                session,
                message: MessageId::new(),
                call: hya_proto::ToolCallId::new(),
                tool: agent.into(),
                input: json!({}),
            })
            .await;
        assert!(
            matches!(before, ToolExecuteBeforeOutcome::Continue { input }
                if input == if agent == "alpha" { json!({"guarded": true}) } else { json!({}) })
        );
        let ToolExecuteAfterOutcome::Continue { result: after } = hooks
            .tool_execute_after(ToolExecuteAfterInput {
                session,
                message: MessageId::new(),
                call: hya_proto::ToolCallId::new(),
                tool: agent.into(),
                input: json!({}),
                result: ToolOutcomeNative::Ok {
                    output: json!("original"),
                    time_ms: 0,
                },
            })
            .await;
        assert!(matches!(after, ToolOutcomeNative::Ok { output, .. }
                if output == if agent == "beta" { json!("after") } else { json!("original") }));
        let result = tools[0].tool.execute(&ctx, json!({})).await;
        handle.shutdown().await.unwrap();
        assert_eq!(result.unwrap()["output"], format!("{agent} result"));
    }
    assert!(std::fs::read_dir(&staging).unwrap().next().is_none());
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

struct ToolViewProvider {
    inner: FakeProvider,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for ToolViewProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream(request, session, message).await
    }
}

#[tokio::test]
async fn one_bun_module_enforces_disjoint_views_in_engine_turns() {
    let _env = StableEnvGuard::new();
    disjoint_engine_turns(false).await;
}

#[tokio::test]
async fn one_bundle_process_enforces_disjoint_views_in_engine_turns() {
    let _env = StableEnvGuard::new();
    disjoint_engine_turns(true).await;
}

async fn disjoint_engine_turns(process: bool) {
    let root = tempdir();
    let staging = tempdir();
    let extensions = if process {
        "  files: [{ id: main, path: main.py }]\n  process: {kind: rust, command: [python3, '${BUNDLE_ROOT}/main.py']}"
    } else {
        "  js: [{ id: main, path: main.js }]"
    };
    let tool_path = if process { "tool.json" } else { "main.js" };
    let manifest = format!(
        r#"kind: Bundle
identity: {{id: test/disjoint-turns, version: 1.0.0, publisher: test}}
agents:
  - {{id: alpha, role: main, resource_view: {{allow: [alpha]}}}}
  - {{id: beta, role: main, resource_view: {{allow: [beta]}}}}
resources:
  tools:
    - {{id: alpha, path: {tool_path}}}
    - {{id: beta, path: {tool_path}}}
extensions:
{extensions}
"#
    );
    let prepared = prepare_package(BundleSource::new("disjoint-turns", vec![
            SourceFile::new("bundle.yaml", manifest),
            SourceFile::new("tool.json", r#"{"type":"object"}"#),
            SourceFile::new("main.js", r#"
export default {server: async () => ({tool: {
  alpha: {description: "alpha", execute: async () => `alpha:${process.pid}`},
  beta: {description: "beta", execute: async () => `beta:${process.pid}`},
}})};
"#),
            SourceFile::new("main.py", r#"
import json, os, sys
for line in sys.stdin:
    req = json.loads(line)
    if req['method'] == 'initialize':
        result = {'protocol_version': 1, 'plugin': {'id': 'disjoint-turns', 'version': '1', 'kind': 'rust'},
                  'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in ['alpha', 'beta']]}
    elif req['method'] == 'tool/call':
        result = {'ok': True, 'output': {'title': '', 'output': req['params']['tool'] + ':' + str(os.getpid()), 'metadata': {}}, 'time_ms': 0}
    elif req['method'] == 'shutdown':
        print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': {}}), flush=True)
        break
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}), flush=True)
"#),
        ])).unwrap();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(prepared.bundles()).unwrap(),
    ));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let bundle = &prepared.bundles()[0];
    let process_extension = prepared.bundle_process("test/disjoint-turns");
    let config = crate::bundle_config::bundle_config_location(
        &root.join("config.yaml"),
        "test/disjoint-turns",
        crate::bundle_config::BundleConfigScope::User,
    )
    .unwrap();
    let source = crate::bundle_runtime::prepare_source(
        bundle,
        crate::bundle_runtime::BundleRuntimeParts {
            process: process_extension,
            schemas: &[],
            apis: &[],
            permission_modes: &[],
            reads: None,
        },
        &crate::bundle_runtime::BundleRuntimeConfig::capture(bundle, process_extension, config),
    )
    .await
    .unwrap();
    runtime
        .refresh(|candidate| candidate.upsert_sources(vec![source.source]))
        .unwrap();
    let mut process_ids = Vec::new();
    for (agent, other) in [("alpha", "beta"), ("beta", "alpha")] {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let scripts = [
            agent.to_string(),
            other.to_string(),
            format!("bundle:test/disjoint-turns/tool/{other}"),
        ]
        .into_iter()
        .map(|name| {
            vec![
                FakeStep::ToolCall {
                    name,
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .chain([vec![FakeStep::Finish(FinishReason::Stop)]])
        .collect();
        let provider = ToolViewProvider {
            inner: FakeProvider::scripted_turns(scripts),
            requests: requests.clone(),
        };
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            "*",
            Mode::Allow,
        )]));
        let environment = BundleSidecarEnvironment::from_command(
            plugins::bundle_sidecar_command().unwrap(),
            staging.clone(),
        );
        let engine = SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            runtime.clone(),
            permission,
            EventBus::default(),
        )
        .with_sidecar_environment(Arc::new(environment));
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new(agent),
                model: ModelRef::new("fake"),
                workdir: root.to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        engine
            .admit_user_prompt(session, "exercise this agent's tool view".into())
            .await
            .unwrap();
        engine
            .run_turn(
                session,
                &AgentSpec {
                    name: AgentName::new(agent),
                    model: ModelRef::new("fake"),
                    system_prompt: "tool view test".into(),
                    workdir: root.clone(),
                    reasoning: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 4);
            for request in requests.iter() {
                let names = request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                assert!(names.contains(&agent), "selected tool missing: {names:?}");
                assert!(
                    !names.contains(&other),
                    "other agent's tool advertised: {names:?}"
                );
                assert!(
                    !names.contains(&format!("bundle:test/disjoint-turns/tool/{other}").as_str())
                );
            }
        }
        let events = engine.store().replay(session).await.unwrap();
        let results = events
            .iter()
            .filter_map(|event| match &event.event {
                Event::ToolResult { output, .. } => Some(output["output"].as_str().unwrap()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "only the selected tool may execute");
        let (tool, pid) = results[0].split_once(':').unwrap();
        assert_eq!(tool, agent);
        process_ids.push(pid.to_string());
        let errors = events
            .iter()
            .filter_map(|event| match &event.event {
                Event::ToolError { message_text, .. } => Some(message_text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            errors,
            [
                format!("unknown tool: {other}"),
                format!("unknown tool: bundle:test/disjoint-turns/tool/{other}")
            ]
        );
    }
    if process {
        assert_eq!(
            process_ids[0], process_ids[1],
            "both turns must use one shared process"
        );
    } else {
        assert_ne!(
            process_ids[0], process_ids[1],
            "private activations must remain session-local"
        );
    }
    drop(runtime);
    assert!(std::fs::read_dir(&staging).unwrap().next().is_none());
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn bun_sidecar_rejects_exports_declared_only_in_an_unloaded_module() {
    let _env = StableEnvGuard::new();
    let mut bundle = disjoint_materialized_bundle("bun-generic-superset");
    set_materialized_extension_content_for_path(
        &mut bundle,
        "extensions/alpha.js",
        r#"
export default {
  id: "alpha-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("alpha extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "alpha echo",
          execute: async () => "alpha",
        },
        beta: {
          description: "unselected beta",
          execute: async () => "beta",
        },
      },
      event: async () => {},
    }
  },
}
"#
        .to_string(),
    );

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("generic Bun superset fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&catalog));
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture generic Bun superset turn binding");
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let factory = environment
        .factory_for(&binding, "alpha")
        .expect("resolve generic Bun superset sidecar factory")
        .expect("alpha must expose a Bun sidecar factory");
    let activation_id = "bun-generic-superset";
    let outcome = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await;
    let rejected = match outcome {
        Err(CoreError::Invalid(_)) => true,
        Ok(mut handle) => {
            let _ = handle.shutdown().await;
            false
        }
        Err(error) => panic!("unexpected generic Bun superset error: {error}"),
    };
    assert!(
        rejected,
        "an export declared only in an unloaded module must fail initialization"
    );
    assert!(
        !staging_root.join(activation_id).exists(),
        "rejected activation must remove its activation directory"
    );
    assert!(
        std::fs::read_dir(&staging_root)
            .expect("read generic Bun superset staging root")
            .next()
            .is_none(),
        "rejected activation must leave staging root empty"
    );

    std::fs::remove_dir_all(&staging_root).expect("cleanup generic Bun staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup generic Bun turn directory");
}

#[tokio::test]
async fn transient_bun_bundle_runs_harness_tool_loop_and_reaps_sidecar() {
    let _env = StableEnvGuard::new();
    let canonical = "bundle:hya/materialized/tool/echo";
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bun_bundle("bun-e2e")]))
            .expect("Bun E2E fixture catalog"),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("bundle complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory()
            .await
            .expect("connect E2E store"),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
        permission,
        EventBus::default(),
    ));
    let workdir = tempdir();
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .expect("create E2E lead");
    let binding = engine.bind_runtime(&workdir).expect("capture E2E binding");
    let staging_root = tempdir();
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        SpawnMember {
            description: "bun e2e".to_string(),
            prompt: "run bundle tool".to_string(),
            subagent_type: "worker".to_string(),
            ..SpawnMember::default()
        },
    )
    .expect("resolve Bun E2E spawn member");
    let ResolvedSpawnMember {
        request,
        agent,
        binding,
        agents,
        resources,
        guidance,
        sidecar_factory,
        ..
    } = resolved;
    let spec = ResidentTurn {
        agent,
        binding,
        agents,
        resources: Some(resources),
        guidance,
        directive: request.prompt,
        description: request.description,
        sidecar_factory,
        tool_call: None,
    };

    let children = run_residents(engine.clone(), lead, vec![spec], Default::default()).await;
    assert_eq!(children.len(), 1);

    let lead_projection = engine
        .read_projection(lead)
        .await
        .expect("read E2E lead projection");
    let child = lead_projection.session.members[0]
        .child
        .expect("E2E member child session");
    let lead_events = engine.store().replay(lead).await.expect("replay E2E lead");
    let finished = lead_events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::MemberFinished {
                    status: MemberRunStatus::Cancelled,
                    child: Some(event_child),
                    ..
                } if *event_child == child
            )
        })
        .count();
    assert_eq!(finished, 1);

    let child_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay E2E child");
    assert!(
        child_events.iter().any(
            |e| matches!(&e.event, Event::TextReplace { text, .. } if text == "bundle complete")
        )
    );
    assert!(child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. }
                if output == &json!({"title": "", "output": "bundle-e2e", "metadata": {}})
        )
    }));
    assert!(
        !child_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
    );
    assert!(
        std::fs::read_dir(&staging_root)
            .expect("read E2E staging root")
            .next()
            .is_none(),
        "resident ending must remove its activation directory"
    );

    std::fs::remove_dir_all(staging_root).expect("cleanup E2E staging root");
    std::fs::remove_dir_all(workdir).expect("cleanup E2E workdir");
}

#[tokio::test]
async fn selected_main_bun_bundle_runs_harness_tool_hook_event_loop_and_reaps_sidecar() {
    let _env = StableEnvGuard::new();
    let canonical = "bundle:hya/materialized/tool/echo";
    let mut bundle = materialized_bun_bundle("bun-root-hooks");
    let event_hook =
        materialized_resource("bun-root-hooks", "hook", "event", "extensions/runtime.js");
    let before_hook = materialized_resource(
        "bun-root-hooks",
        "hook",
        "tool.execute.before",
        "extensions/runtime.js",
    );
    let after_hook = materialized_resource(
        "bun-root-hooks",
        "hook",
        "tool.execute.after",
        "extensions/runtime.js",
    );
    bundle.agents[0].hook_refs = vec![
        event_hook.stable_id.clone(),
        before_hook.stable_id.clone(),
        after_hook.stable_id.clone(),
    ];
    bundle.hooks.extend([event_hook, before_hook, after_hook]);
    bundle.agents[0].id = AgentName::new("root-hook-agent");
    bundle.agents[0].role = AgentRole::Main;
    set_materialized_extension_content(
        &mut bundle,
        r#"
let eventCount = 0
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      event: () => {
        eventCount += 1
      },
      "tool.execute.before": async (params) => {
        if (eventCount <= 0 || params.input === null || typeof params.input !== "object") {
          throw new Error("bundle hook received unexpected tool input")
        }
        return { input: { ...params.input, hooked: true, eventCount } }
      },
      "tool.execute.after": async (params) => ({
        ...params.result,
        output: { ...params.result.output, output: `${params.result.output.output}:after` },
      }),
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (
              input === null ||
              typeof input !== "object" ||
              input.hooked !== true ||
              !Number.isInteger(input.eventCount) ||
              input.eventCount <= 0
            ) {
              throw new Error("bundle echo did not receive hook mutation")
            }
            return `bun-root-hooks:${input.eventCount}`
          },
        },
      },
    }
  },
}
"#
        .to_string(),
    );
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("root Bun hook fixture catalog"),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("bundle root complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let workdir = tempdir();
    let staging_root = tempdir();
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory()
                .await
                .expect("connect root Bun hook store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
            permission,
            EventBus::default(),
        )
        .with_sidecar_environment(Arc::new(environment)),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("root-hook-agent"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .expect("create root Bun hook session");
    engine
        .admit_user_prompt(session, "run root bundle hook".to_string())
        .await
        .expect("admit root Bun hook prompt");
    let finish = engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("root-hook-agent"),
                model: ModelRef::new("fake"),
                system_prompt: "root base".to_string(),
                workdir: workdir.clone(),
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect("run root Bun hook turn");
    assert_eq!(finish, FinishReason::Stop);

    let events = engine
        .store()
        .replay(session)
        .await
        .expect("replay root Bun hook turn");
    let successful_results = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(successful_results.len(), 1);
    let output = successful_results[0]
        .get("output")
        .and_then(Value::as_str)
        .expect("root Bun hook tool result output must be a string");
    assert!(output.starts_with("bun-root-hooks:"));
    assert!(output.ends_with(":after"));
    assert!(
        !events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
    );
    assert!(
        std::fs::read_dir(&staging_root)
            .expect("read root Bun hook staging root")
            .next()
            .is_none(),
        "transient root sidecar shutdown must remove its activation directory"
    );

    std::fs::remove_dir_all(staging_root).expect("cleanup root Bun hook staging root");
    std::fs::remove_dir_all(workdir).expect("cleanup root Bun hook workdir");
}

#[tokio::test]
async fn resident_bun_bundle_reuses_one_sidecar_across_two_mailbox_turns() {
    let _env = StableEnvGuard::new();
    let canonical = "bundle:hya/materialized/tool/echo";
    let mut bundle = materialized_bun_bundle("bun-resident");
    set_materialized_extension_content(
        &mut bundle,
        r#"
let calls = 0
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
              throw new Error("bundle echo received unexpected tool input")
            }
            return `${process.pid}:${++calls}`
          },
        },
      },
    }
  },
}
"#
        .to_string(),
    );
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("resident Bun fixture catalog"),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("resident first complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("resident second complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory()
            .await
            .expect("connect resident E2E store"),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
        permission,
        EventBus::default(),
    ));
    let workdir = tempdir();
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .expect("create resident E2E lead");
    let binding = engine
        .bind_runtime(&workdir)
        .expect("capture resident E2E binding");
    let staging_root = tempdir();
    let command = plugins::bundle_sidecar_command().expect("Bun must be available");
    let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        SpawnMember {
            subagent_type: "worker".to_string(),
            ..SpawnMember::default()
        },
    )
    .expect("resolve resident Bun spawn member");
    let ResolvedSpawnMember {
        agent,
        binding,
        agents,
        resources,
        sidecar_factory,
        ..
    } = resolved;
    let sidecar_factory = sidecar_factory.expect("resident Bun must capture sidecar factory");
    let _lead_turn = engine
        .try_begin_turn(lead)
        .expect("hold the calling lead turn");
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            lead,
            agent,
            (binding, agents, resources, Some(sidecar_factory)),
            String::new(),
            None,
            None,
        )
        .await
        .expect("spawn resident Bun member");
    let mut bus = engine.bus().subscribe();

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident mail one".to_string(),
        )
        .await
        .expect("send first resident mail");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = bus.recv().await.expect("resident bus remains open");
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    handle: event_handle,
                    status: RosterStatus::Idle,
                    ..
                } if event_handle == &handle
            ) {
                break;
            }
        }
    })
    .await
    .expect("first resident mailbox turn must become idle");
    let first_projection = engine
        .read_projection(lead)
        .await
        .expect("read first resident roster");
    assert_eq!(
        first_projection
            .team
            .roster
            .get(&handle)
            .expect("resident roster entry")
            .status,
        RosterStatus::Idle
    );
    let first_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay first resident child");
    let first_outputs = first_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(first_outputs.len(), 1);
    let first_output = first_outputs[0]
        .get("output")
        .and_then(Value::as_str)
        .expect("resident tool result output must be a string");
    let (pid, counter) = first_output
        .split_once(':')
        .expect("resident tool output must contain pid and counter");
    assert!(!pid.is_empty());
    assert_eq!(counter, "1");
    assert!(
        !first_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
    );

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident mail two".to_string(),
        )
        .await
        .expect("send second resident mail");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = bus.recv().await.expect("resident bus remains open");
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    handle: event_handle,
                    status: RosterStatus::Idle,
                    ..
                } if event_handle == &handle
            ) {
                break;
            }
        }
    })
    .await
    .expect("second resident mailbox turn must become idle");
    let second_projection = engine
        .read_projection(lead)
        .await
        .expect("read second resident roster");
    assert_eq!(
        second_projection
            .team
            .roster
            .get(&handle)
            .expect("resident roster entry")
            .status,
        RosterStatus::Idle
    );
    let second_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay second resident child");
    let second_outputs = second_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(second_outputs.len(), 2);
    let second_output = second_outputs[1]
        .get("output")
        .and_then(Value::as_str)
        .expect("resident tool result output must be a string");
    let (second_pid, second_counter) = second_output
        .split_once(':')
        .expect("resident tool output must contain pid and counter");
    assert_eq!(second_pid, pid);
    assert_eq!(second_counter, "2");
    assert!(
        !second_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
    );

    supervisor
        .team_cancel(lead)
        .expect("resident team cancel token")
        .cancel();
    std::fs::remove_dir_all(staging_root).expect("cleanup resident staging root");
    std::fs::remove_dir_all(workdir).expect("cleanup resident workdir");
}

#[tokio::test]
async fn resident_idle_sidecar_loss_restarts_before_next_mail() {
    let env_root = tempdir();
    let workdir = env_root.join("agent-workdir");
    let env_guard = EnvGuard::with_home(&env_root, &workdir);
    let canonical = "bundle:hya/materialized/tool/echo";
    let bundle = materialized_tool_bundle("bun-resident-loss");
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("resident loss fixture catalog"),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("resident loss first complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("resident loss second complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory()
            .await
            .expect("connect resident loss store"),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
        permission,
        EventBus::default(),
    ));
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .expect("create resident loss lead");
    let binding = engine
        .bind_runtime(&workdir)
        .expect("capture resident loss binding");
    let old_generation = binding.generation();
    let staging_root = tempdir();
    let socket_path = temp_socket("sl");
    let listener =
        std::os::unix::net::UnixListener::bind(&socket_path).expect("bind sidecar loss listener");
    let socket_literal = serde_json::to_string(socket_path.to_string_lossy().as_ref())
        .expect("encode sidecar loss socket path");
    let fixture = r#"
import json, os, socket, sys
socket_path = __SOCKET__
first_call = True
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "tool/call" and first_call:
        first_call = False
        result = {
            "ok": True,
            "output": {"title": "", "output": str(os.getpid()), "metadata": {}},
            "time_ms": 0
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(socket_path)
        sock.sendall(b"x")
        sock.recv(1)
        sys.stdout.close()
        sock.close()
        break
"#
        .replace("__SOCKET__", &socket_literal);
    let terminate_notify = Arc::new(tokio::sync::Notify::new());
    let mut environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture],
        staging_root.clone(),
    );
    environment.terminate_notify = Some(Arc::clone(&terminate_notify));
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        SpawnMember {
            subagent_type: "worker".to_string(),
            ..SpawnMember::default()
        },
    )
    .expect("resolve resident loss member");
    let ResolvedSpawnMember {
        agent,
        binding,
        agents,
        resources,
        sidecar_factory,
        ..
    } = resolved;
    let _lead_turn = engine
        .try_begin_turn(lead)
        .expect("hold the calling lead turn");
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            lead,
            agent,
            (
                binding,
                agents,
                resources,
                Some(sidecar_factory.expect("resident loss sidecar factory")),
            ),
            String::new(),
            None,
            None,
        )
        .await
        .expect("spawn resident loss member");
    let mut bus = engine.bus().subscribe();

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident loss mail one".to_string(),
        )
        .await
        .expect("send first resident loss mail");
    loop {
        let envelope = bus.recv().await.expect("resident loss bus remains open");
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                handle: event_handle,
                status: RosterStatus::Idle,
                ..
            } if event_handle == &handle
        ) {
            break;
        }
    }
    let first_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay first resident loss turn");
    assert_eq!(
        first_events
            .iter()
            .filter(|envelope| matches!(&envelope.event, Event::ToolResult { .. }))
            .count(),
        1
    );

    let first_loss_listener = listener.try_clone().expect("clone sidecar loss listener");
    let first_loss = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            use std::io::{Read as _, Write as _};
            let (mut stream, _) = first_loss_listener.accept()?;
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte)?;
            stream.write_all(b"y")?;
            Ok::<u8, std::io::Error>(byte[0])
        }),
    )
    .await
    .expect("sidecar loss must be announced promptly")
    .expect("sidecar loss listener task must join")
    .expect("sidecar loss marker must be readable");
    assert_eq!(first_loss, b'x');
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        terminate_notify.notified(),
    )
    .await
    .expect("resident sidecar terminate cleanup must complete");

    let replacement = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle(
            "resident-replacement",
        )]))
        .expect("replacement resident catalog"),
    ));
    let replacement_generation = engine
        .runtime_registry()
        .publish_catalog(replacement)
        .expect("publish replacement catalog");
    assert_ne!(replacement_generation, old_generation);
    let fresh_binding = engine
        .runtime_registry()
        .bind_turn(&workdir)
        .expect("bind replacement resident generation");
    assert_eq!(fresh_binding.generation(), replacement_generation);

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident loss mail two".to_string(),
        )
        .await
        .expect("send second resident loss mail");
    let post_mail_current_task = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut second_mail_sent = false;
        loop {
            let envelope = bus.recv().await.expect("resident loss bus remains open");
            if matches!(
                &envelope.event,
                Event::MailSent { body, .. } if body == "resident loss mail two"
            ) {
                second_mail_sent = true;
            }
            if let Event::AgentActivityChanged {
                handle: event_handle,
                status,
                current_task,
                ..
            } = &envelope.event
                && second_mail_sent
                && event_handle == &handle
                && matches!(status, RosterStatus::Idle | RosterStatus::Failed)
            {
                break current_task.clone();
            }
        }
    })
    .await
    .expect("replacement resident must reach Idle or Failed after second mail");
    let second_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay second resident loss turn");
    let successful_results = second_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        successful_results.len(),
        2,
        "current_task={post_mail_current_task:?}\n{second_events:#?}"
    );
    assert!(
        !second_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
    );
    let pids = successful_results
        .iter()
        .map(|result| {
            result
                .get("output")
                .and_then(Value::as_str)
                .expect("resident loss tool result output must be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(pids.iter().all(|pid| !pid.is_empty()));
    assert_ne!(pids[0], pids[1]);
    let recorded_generations = second_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(recorded_generations.len() >= 2);
    assert!(
        recorded_generations
            .iter()
            .all(|generation| *generation == old_generation),
        "resident turns must retain their captured binding generation"
    );

    supervisor
        .team_cancel(lead)
        .expect("resident loss team cancel token")
        .cancel();
    drop(env_guard);
    std::fs::remove_dir_all(staging_root).expect("cleanup resident loss staging root");
    std::fs::remove_dir_all(env_root).expect("cleanup resident loss env root");
}

#[tokio::test]
async fn resident_running_sidecar_loss_reports_failure_before_mail_revives_new_epoch() {
    let env_root = tempdir();
    let workdir = env_root.join("agent-workdir");
    let env_guard = EnvGuard::with_home(&env_root, &workdir);
    let canonical = "bundle:hya/materialized/tool/echo";
    let bundle = materialized_tool_bundle("bun-resident-running-loss");
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("resident running loss fixture catalog"),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("resident queued complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory()
            .await
            .expect("connect resident running loss store"),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
        permission,
        EventBus::default(),
    ));
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .expect("create resident running loss lead");
    let binding = engine
        .bind_runtime(&workdir)
        .expect("capture resident running loss binding");
    let old_generation = binding.generation();
    let staging_root = tempdir();
    let marker_socket = temp_socket("rrlm");
    let release_socket = temp_socket("rrlr");
    let marker_listener = std::os::unix::net::UnixListener::bind(&marker_socket)
        .expect("bind resident running loss marker listener");
    let release_listener = std::os::unix::net::UnixListener::bind(&release_socket)
        .expect("bind resident running loss release listener");
    let marker_literal = serde_json::to_string(marker_socket.to_string_lossy().as_ref())
        .expect("encode resident running loss marker socket");
    let release_literal = serde_json::to_string(release_socket.to_string_lossy().as_ref())
        .expect("encode resident running loss release socket");
    let fixture = r#"
import json, os, socket, sys
marker_socket = __MARKER_SOCKET__
release_socket = __RELEASE_SOCKET__
def send_marker(label):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(marker_socket)
    sock.sendall((label + ":" + str(os.getpid()) + "\n").encode())
    sock.close()
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "tool/call":
        send_marker("first")
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(release_socket)
        sock.recv(1)
        sock.close()
        sys.stdout.close()
        break
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#
        .replace("__MARKER_SOCKET__", &marker_literal)
        .replace("__RELEASE_SOCKET__", &release_literal);
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture],
        staging_root.clone(),
    );
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "lead base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        SpawnMember {
            subagent_type: "worker".to_string(),
            ..SpawnMember::default()
        },
    )
    .expect("resolve resident running loss member");
    let ResolvedSpawnMember {
        agent,
        binding,
        agents,
        resources,
        sidecar_factory,
        ..
    } = resolved;
    let _lead_turn = engine
        .try_begin_turn(lead)
        .expect("hold the calling lead turn");
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            lead,
            agent,
            (
                binding,
                agents,
                resources,
                Some(sidecar_factory.expect("resident running loss sidecar factory")),
            ),
            String::new(),
            None,
            None,
        )
        .await
        .expect("spawn resident running loss member");
    let mut bus = engine.bus().subscribe();
    let first_marker_listener = marker_listener
        .try_clone()
        .expect("clone resident running loss marker listener");
    let first_marker_task = tokio::task::spawn_blocking(move || {
        use std::io::BufRead as _;
        let (stream, _) = first_marker_listener.accept()?;
        let mut line = String::new();
        std::io::BufReader::new(stream).read_line(&mut line)?;
        Ok::<String, std::io::Error>(line)
    });
    let release_task = tokio::task::spawn_blocking(move || {
        let (stream, _) = release_listener.accept()?;
        Ok::<std::os::unix::net::UnixStream, std::io::Error>(stream)
    });

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident running mail one".to_string(),
        )
        .await
        .expect("send first resident running mail");
    let first_marker = tokio::time::timeout(std::time::Duration::from_secs(5), first_marker_task)
        .await
        .expect("first resident running marker must arrive")
        .expect("first resident running marker task must join")
        .expect("first resident running marker must be readable");
    let (first_label, _first_pid) = first_marker
        .trim()
        .split_once(':')
        .expect("first resident running marker must contain label and pid");
    assert_eq!(first_label, "first");

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident running mail two".to_string(),
        )
        .await
        .expect("send queued resident running mail");
    // Revival of executable capabilities is M4-6; this new episode needs
    // only the replacement agent's prompt and binding.
    let mut replacement_bundle = materialized_bundle("resident-running-replacement");
    replacement_bundle.tools.clear();
    replacement_bundle.hooks.clear();
    replacement_bundle.extensions.clear();
    replacement_bundle.agents[0].resource_view = ResourceView::default();
    replacement_bundle.agents[0].hook_refs.clear();
    let replacement = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([replacement_bundle]))
            .expect("replacement resident running catalog"),
    ));
    let replacement_generation = engine
        .runtime_registry()
        .publish_catalog(replacement)
        .expect("publish replacement resident running catalog");
    assert_ne!(replacement_generation, old_generation);
    let fresh_binding = engine
        .runtime_registry()
        .bind_turn(&workdir)
        .expect("bind replacement resident running generation");
    assert_eq!(fresh_binding.generation(), replacement_generation);

    let mut release_stream = tokio::time::timeout(std::time::Duration::from_secs(5), release_task)
        .await
        .expect("first resident sidecar release connection must arrive")
        .expect("resident sidecar release task must join")
        .expect("resident sidecar release connection must be accepted");
    use std::io::Write as _;
    release_stream
        .write_all(b"x")
        .expect("release first resident sidecar exactly once");

    tokio::time::timeout(Duration::from_secs(11), async {
            loop {
                let envelope = bus.recv().await.expect("resident running bus remains open");
                if matches!(envelope.event, Event::AgentArchived { child: archived, .. } if archived == child) {
                    break;
                }
            }
        }).await.expect("running loss must end the episode");
    assert_eq!(supervisor.episode_count(), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let root_events = engine.replay(lead).await.unwrap();
    assert_eq!(root_events.iter().filter(|e| matches!(&e.event,
            Event::SubagentReported { child: reported, outcome: hya_proto::ReportOutcome::Failed, report, .. }
            if *reported == child && report.contains("activation sidecar lost")
        )).count(), 1);
    let root = engine.read_projection(lead).await.unwrap();
    assert_eq!(
        root.team.archived[&handle].reason,
        hya_proto::ArchiveReason::Reported
    );
    assert_eq!(
        root_events
            .iter()
            .filter(|e| matches!(&e.event,
                Event::ResidentWorkStarted { actor_session, .. } if *actor_session == child
            ))
            .count(),
        1,
        "queued mail must not recover the failed episode in process"
    );

    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "new episode after resident sidecar loss".into(),
        )
        .await
        .expect("mail must revive the failed member");
    let revived = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let envelope = bus.recv().await.expect("revived resident bus remains open");
                if matches!(envelope.event, Event::MessageFinished { session, role: hya_proto::Role::Assistant, finish: FinishReason::Stop, .. } if session == child) {
                    break;
                }
            }
        }).await;
    assert!(
        revived.is_ok(),
        "revived episode failed: {:?}",
        engine.read_projection(child).await.unwrap()
    );

    let child_events = engine
        .store()
        .replay(child)
        .await
        .expect("replay resident running child");
    let durable_finishes = child_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::MessageFinished {
                session,
                role: hya_proto::Role::Assistant,
                finish,
                ..
            } if *session == child => Some(*finish),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        durable_finishes,
        vec![FinishReason::Cancelled, FinishReason::Stop],
        "durable child finishes must preserve the failed episode before revival"
    );
    let assistant_messages = child_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::MessageStarted {
                session,
                message,
                role: hya_proto::Role::Assistant,
                ..
            } if *session == child => Some(*message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(assistant_messages.len() >= 2);
    let first_message = assistant_messages[0];
    let first_step_count = child_events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::StepStarted {
                    session,
                    message,
                    ..
                } if *session == child && *message == first_message
            )
        })
        .count();
    assert_eq!(
        first_step_count, 1,
        "stale running mail must not continue polling the model"
    );

    let tool_results = child_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        tool_results.is_empty(),
        "the lost sidecar must not publish a successful tool result"
    );

    let tool_errors = child_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolError {
                value,
                message_text,
                ..
            } => Some((value, message_text)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_errors.len(), 1);
    // Sidecar loss cancels the running turn; the tool reports its own
    // cancelled result before the episode releases its unchanged claim.
    assert_eq!(
        tool_errors[0]
            .0
            .as_ref()
            .and_then(|value| value.pointer("/error/type"))
            .and_then(Value::as_str),
        Some("cancelled")
    );
    assert!(!tool_errors[0].1.to_ascii_lowercase().contains("closed"));

    let root_events = engine
        .store()
        .replay(lead)
        .await
        .expect("replay resident running root");
    let work_started = root_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ResidentWorkStarted {
                actor_session,
                epoch,
                inbox_through,
                ..
            } if *actor_session == child => Some((*epoch, *inbox_through)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(work_started.len(), 2);
    assert!(work_started[0].0 < work_started[1].0);
    assert_eq!(
        work_started
            .iter()
            .map(|(_, inbox_through)| *inbox_through)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    let recorded_generations = child_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        recorded_generations,
        vec![old_generation, replacement_generation],
        "revival starts a new episode with the current binding"
    );
    assert_eq!(
        root_events
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MailSent { body, .. } if body == "resident running mail two"
                )
            })
            .count(),
        1,
        "queued mail must be represented exactly once"
    );

    supervisor
        .end_member(lead, &handle)
        .await
        .expect("archive revived resident");
    if let Some(cancel) = supervisor.team_cancel(lead) {
        cancel.cancel();
    }
    drop(env_guard);
    std::fs::remove_dir_all(staging_root).expect("cleanup resident running loss staging root");
    std::fs::remove_dir_all(env_root).expect("cleanup resident running loss env root");
}

#[tokio::test]
async fn bundle_sidecar_rejects_task_context_hook_before_ack() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("unsupported-hook")]))
            .expect("unsupported hook fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture unsupported hook turn binding");
    let staging_root = tempdir();
    let fixture = activation_sidecar_fixture(r#"[{"name": "message.user.before"}]"#);
    let environment = BundleSidecarEnvironment::from_command(
        vec!["python3".to_string(), "-c".to_string(), fixture],
        staging_root.clone(),
    );
    let factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve unsupported hook sidecar factory")
        .expect("materialized Bundle must expose its sidecar factory");
    let activation_id = "activation-unsupported-hook";
    let outcome = factory
        .start(SidecarStart {
            activation_id: activation_id.to_string(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await;
    let detail = match outcome {
        Ok(handle) => {
            drop(handle);
            None
        }
        Err(CoreError::Invalid(detail)) => Some(detail),
        Err(error) => Some(format!("unexpected error: {error}")),
    };
    let activation_removed = !staging_root.join(activation_id).exists();
    std::fs::remove_dir_all(&staging_root).expect("cleanup unsupported hook staging root");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup unsupported hook turn directory");

    let detail = detail.expect("unsupported Bundle sidecar hook declaration must fail");
    assert!(
        detail.contains("unsupported Bundle sidecar hook declaration `message.user.before`"),
        "unexpected unsupported hook error: {detail}"
    );
    assert!(
        activation_removed,
        "unsupported hook rejection must remove its activation directory"
    );
}

#[test]
fn bundle_sidecar_duplicate_hook_declaration_is_rejected() {
    let _env = StableEnvGuard::new();
    let registrations = [
        HookRegistration {
            name: HookName::Event,
        },
        HookRegistration {
            name: HookName::Event,
        },
    ];
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog_with_agents(&[]));
    let binding = runtime
        .bind_turn(&std::env::temp_dir())
        .expect("bind duplicate hook fixture");
    let result = validate_bundle_sidecar_hooks(&binding, "build", &registrations);
    let detail = match result {
        Err(CoreError::Invalid(detail)) => detail,
        Ok(_) => "validator returned Ok".to_string(),
        Err(error) => format!("unexpected error: {error}"),
    };
    assert!(
        detail.contains("duplicate Bundle sidecar hook declaration `event`"),
        "duplicate Event declaration must be rejected: {detail}"
    );
}

fn materialized_resource(
    marker: &str,
    kind: &str,
    local_id: &str,
    source_path: &str,
) -> PreparedResource {
    PreparedResource {
        local_id: local_id.to_string(),
        stable_id: format!("bundle:hya/materialized/{kind}/{local_id}"),
        source_path: source_path.to_string(),
        digest: format!("{marker}-{source_path}-digest"),
        content: format!("{marker} {source_path}\n"),
        binary_base64: None,
        aliases: Vec::new(),
    }
}

fn materialized_bundle(marker: &str) -> Bundle {
    let mut resource_view = ResourceView::default();
    resource_view.allow.push("echo".to_string());
    Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/materialized".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: format!("{marker}-bundle-digest"),
        agents: vec![PreparedAgent {
            id: AgentName::new("worker"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{marker} worker prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view,
            can_spawn: Vec::new(),
            hook_refs: vec!["bundle:hya/materialized/hook/event".to_string()],
            advertise: Vec::new(),
        }],
        tools: vec![materialized_resource(
            marker,
            "tool",
            "echo",
            "extensions/runtime.js",
        )],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: vec![materialized_resource(
            marker,
            "hook",
            "event",
            "extensions/runtime.js",
        )],
        extensions: vec![materialized_resource(
            marker,
            "extension",
            "runtime",
            "extensions/runtime.js",
        )],

        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    }
}

fn materialized_tool_bundle(marker: &str) -> Bundle {
    let mut bundle = materialized_bundle(marker);
    bundle.hooks.clear();
    bundle.agents[0].hook_refs.clear();
    bundle
}

/// Two bundles with disjoint closures: `alpha` and `beta`, each owning its
/// own tool, hook, and extension.
fn disjoint_materialized_bundles(marker: &str) -> Vec<Bundle> {
    let alpha = disjoint_materialized_bundle(marker);
    let mut beta = alpha.clone();
    beta.identity.id = "hya/materialized-beta".to_string();
    beta.digest = format!("{marker}-beta-bundle-digest");
    let beta_tool = format!("bundle:{}/tool/beta", beta.identity.id);
    let beta_hook = format!("bundle:{}/hook/tool.execute.before", beta.identity.id);
    beta.tools = vec![PreparedResource {
        stable_id: beta_tool.clone(),
        ..alpha.tools[1].clone()
    }];
    beta.hooks = vec![PreparedResource {
        stable_id: beta_hook.clone(),
        ..alpha.hooks[1].clone()
    }];
    beta.extensions = vec![PreparedResource {
        stable_id: format!("bundle:{}/extension/beta", beta.identity.id),
        ..alpha.extensions[1].clone()
    }];
    beta.agents[0] = PreparedAgent {
        id: AgentName::new("beta"),
        prompt: Some(format!("{marker} beta prompt")),
        resource_view: ResourceView {
            allow: vec![beta_tool],
            ..ResourceView::default()
        },
        hook_refs: vec![beta_hook],
        ..alpha.agents[0].clone()
    };

    let mut alpha_only = alpha;
    alpha_only.tools.truncate(1);
    alpha_only.hooks.truncate(1);
    alpha_only.extensions.truncate(1);
    vec![alpha_only, beta]
}

fn disjoint_materialized_bundle(marker: &str) -> Bundle {
    let alpha_path = "extensions/alpha.js";
    let beta_path = "extensions/beta.js";
    let alpha_tool = materialized_resource(marker, "tool", "echo", alpha_path);
    let alpha_hook = materialized_resource(marker, "hook", "event", alpha_path);
    let alpha_extension = materialized_resource(marker, "extension", "alpha", alpha_path);
    let beta_tool = materialized_resource(marker, "tool", "beta", beta_path);
    let beta_hook = materialized_resource(marker, "hook", "tool.execute.before", beta_path);
    let beta_extension = materialized_resource(marker, "extension", "beta", beta_path);
    Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/materialized".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: format!("{marker}-disjoint-bundle-digest"),
        // One agent per bundle: `alpha` selects only its own closure, while
        // the bundle still ships both extensions so selection can be
        // exercised over a superset.
        agents: vec![PreparedAgent {
            id: AgentName::new("alpha"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{marker} alpha prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView {
                allow: vec![alpha_tool.stable_id.clone()],
                ..ResourceView::default()
            },
            can_spawn: Vec::new(),
            hook_refs: vec![alpha_hook.stable_id.clone()],
            advertise: Vec::new(),
        }],
        tools: vec![alpha_tool, beta_tool],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: vec![alpha_hook, beta_hook],
        extensions: vec![alpha_extension, beta_extension],

        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    }
}

/// Two bundles where one tries to select the other's tool and hook.
fn cross_bundle_selector_catalog(marker: &str) -> Arc<AgentCatalog> {
    let owner = materialized_bundle(marker);
    let mut selector = owner.clone();
    selector.identity.id = "hya/selector".to_string();
    selector.digest = "selector-bundle-digest".to_string();
    let mut selector_agent = selector.agents[0].clone();
    selector_agent.id = AgentName::new("selector");
    selector_agent.resource_view = ResourceView {
        allow: vec!["bundle:hya/materialized/tool/echo".to_string()],
        ..ResourceView::default()
    };
    selector_agent.hook_refs = vec!["bundle:hya/materialized/hook/event".to_string()];
    selector.agents[0] = selector_agent;
    selector.tools.clear();
    selector.skills.clear();
    selector.mcp.clear();
    selector.hooks.clear();
    selector.extensions.clear();

    Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([selector, owner]))
            .expect("cross-bundle materialization fixture catalog"),
    ))
}

#[test]
fn a_bundle_cannot_select_another_bundles_tool() {
    let _env = StableEnvGuard::new();
    // A bundle agent's plane admits only its OWN bundle resources. Before
    // this rule, the allow-driven re-resolution searched every bundle, so a
    // selector could borrow the owner's tool.
    let catalog = cross_bundle_selector_catalog("owner");
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture cross-bundle turn binding");

    let refused = binding.has_selected_bundle_sidecar_capability("selector");
    std::fs::remove_dir_all(&turn_dir).expect("cleanup turn directory");

    let Err(hya_bundle::BundleError::ResourceNotInPlane { reference, .. }) = refused else {
        panic!("cross-bundle selection must be refused, got {refused:?}");
    };
    assert_eq!(reference, "bundle:hya/materialized/tool/echo");
}

fn set_materialized_extension_content(bundle: &mut Bundle, content: String) {
    let digest = format!(
        "materialized-content-{:x}",
        Sha256::digest(content.as_bytes())
    );
    for resource in bundle
        .tools
        .iter_mut()
        .chain(bundle.hooks.iter_mut())
        .chain(bundle.extensions.iter_mut())
    {
        resource.content = content.clone();
        resource.digest = digest.clone();
    }
}

fn set_materialized_extension_content_for_path(
    bundle: &mut Bundle,
    source_path: &str,
    content: String,
) {
    let digest = format!(
        "materialized-content-{:x}",
        Sha256::digest(content.as_bytes())
    );
    for resource in bundle
        .tools
        .iter_mut()
        .chain(bundle.hooks.iter_mut())
        .chain(bundle.extensions.iter_mut())
    {
        if resource.source_path == source_path {
            resource.content = content.clone();
            resource.digest = digest.clone();
        }
    }
}

fn materialized_bun_bundle(marker: &str) -> Bundle {
    let mut bundle = materialized_tool_bundle(marker);
    set_materialized_extension_content(
        &mut bundle,
        r#"
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
              throw new Error("bundle echo received unexpected tool input")
            }
            return "bundle-e2e"
          },
        },
      },
    }
  },
}
"#
        .to_string(),
    );
    bundle
}

#[test]
fn sidecar_resources_materialize_from_captured_turn_binding() {
    let _env = StableEnvGuard::new();
    let old_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("old")]))
            .expect("old fixture catalog"),
    ));
    let new_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("new")]))
            .expect("new fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
    let turn_dir = tempdir();
    let old_binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture old turn binding");
    runtime
        .publish_catalog(new_catalog)
        .expect("publish replacement catalog");

    let activation_root = tempdir();
    let activation_dir = activation_root.join("activation");
    std::fs::create_dir_all(&activation_dir).expect("create activation directory");
    let materialized =
        materialize_bundle_sidecar_resources(&old_binding, "worker", &activation_dir)
            .expect("materialize captured bundle resources");

    assert_eq!(
        materialized,
        vec![activation_dir.join("extensions/runtime.js")]
    );
    assert_eq!(
        std::fs::read_to_string(activation_dir.join("extensions/runtime.js"))
            .expect("read materialized extension"),
        "old extensions/runtime.js\n"
    );
    let materialized_files = ["extensions/runtime.js"];
    for relative_path in materialized_files {
        let content = std::fs::read_to_string(activation_dir.join(relative_path))
            .expect("read materialized resource");
        assert!(
            !content.contains("new"),
            "captured resources must not use new catalog"
        );
    }

    std::fs::remove_dir_all(activation_root).expect("cleanup activation directory");
    std::fs::remove_dir_all(turn_dir).expect("cleanup turn directory");
}

#[test]
fn sidecar_materialization_uses_only_captured_agent_selected_closure() {
    let _env = StableEnvGuard::new();
    let old_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles(disjoint_materialized_bundles("old")))
            .expect("old disjoint fixture catalog"),
    ));
    let new_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles(disjoint_materialized_bundles("new")))
            .expect("new disjoint fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
    let old_turn_dir = tempdir();
    let old_binding = runtime
        .bind_turn(&old_turn_dir)
        .expect("capture old disjoint turn binding");
    runtime
        .publish_catalog(new_catalog)
        .expect("publish disjoint replacement catalog");

    let alpha_root = tempdir();
    let alpha_dir = alpha_root.join("activation");
    std::fs::create_dir_all(&alpha_dir).expect("create alpha activation directory");
    let alpha_materialized =
        materialize_bundle_sidecar_resources(&old_binding, "alpha", &alpha_dir)
            .expect("materialize alpha captured resources");
    assert_eq!(
        alpha_materialized,
        vec![alpha_dir.join("extensions/alpha.js")]
    );
    assert_eq!(
        std::fs::read_to_string(alpha_dir.join("extensions/alpha.js"))
            .expect("read alpha materialized extension"),
        "old extensions/alpha.js\n"
    );
    assert!(!alpha_dir.join("extensions/beta.js").exists());

    let beta_root = tempdir();
    let beta_dir = beta_root.join("activation");
    std::fs::create_dir_all(&beta_dir).expect("create beta activation directory");
    let beta_materialized = materialize_bundle_sidecar_resources(&old_binding, "beta", &beta_dir)
        .expect("materialize beta captured resources");
    assert_eq!(beta_materialized, vec![beta_dir.join("extensions/beta.js")]);
    assert_eq!(
        std::fs::read_to_string(beta_dir.join("extensions/beta.js"))
            .expect("read beta materialized extension"),
        "old extensions/beta.js\n"
    );
    assert!(!beta_dir.join("extensions/alpha.js").exists());

    let new_turn_dir = tempdir();
    let new_binding = runtime
        .bind_turn(&new_turn_dir)
        .expect("capture new disjoint turn binding");
    let new_root = tempdir();
    let new_dir = new_root.join("activation");
    std::fs::create_dir_all(&new_dir).expect("create new activation directory");
    let new_materialized = materialize_bundle_sidecar_resources(&new_binding, "alpha", &new_dir)
        .expect("materialize new captured alpha resources");
    assert_eq!(new_materialized, vec![new_dir.join("extensions/alpha.js")]);
    assert_eq!(
        std::fs::read_to_string(new_dir.join("extensions/alpha.js"))
            .expect("read new alpha materialized extension"),
        "new extensions/alpha.js\n"
    );
    assert!(!new_dir.join("extensions/beta.js").exists());

    std::fs::remove_dir_all(alpha_root).expect("cleanup alpha activation directory");
    std::fs::remove_dir_all(beta_root).expect("cleanup beta activation directory");
    std::fs::remove_dir_all(new_root).expect("cleanup new activation directory");
    std::fs::remove_dir_all(old_turn_dir).expect("cleanup old turn directory");
    std::fs::remove_dir_all(new_turn_dir).expect("cleanup new turn directory");
}

#[test]
fn sidecar_materialization_orders_deduplicated_extensions_by_canonical_identity() {
    let _env = StableEnvGuard::new();
    let mut bundle = disjoint_materialized_bundle("ordered");
    bundle.agents[0]
        .resource_view
        .allow
        .push(bundle.tools[1].stable_id.clone());
    bundle.extensions[0].local_id = "zeta".to_string();
    bundle.extensions[0].stable_id = "bundle:hya/materialized/extension/zeta".to_string();
    bundle.extensions[1].local_id = "alpha".to_string();
    bundle.extensions[1].stable_id = "bundle:hya/materialized/extension/alpha".to_string();

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("ordered materialization fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture ordered materialization turn binding");
    let activation_root = tempdir();
    let activation_dir = activation_root.join("activation");
    std::fs::create_dir_all(&activation_dir).expect("create activation directory");

    let materialized = materialize_bundle_sidecar_resources(&binding, "alpha", &activation_dir)
        .expect("materialize ordered captured resources");
    let alpha_content = std::fs::read_to_string(activation_dir.join("extensions/alpha.js"))
        .expect("read alpha extension");
    let beta_content = std::fs::read_to_string(activation_dir.join("extensions/beta.js"))
        .expect("read beta extension");

    std::fs::remove_dir_all(activation_root).expect("cleanup activation directory");
    std::fs::remove_dir_all(turn_dir).expect("cleanup turn directory");

    assert_eq!(
        materialized,
        vec![
            activation_dir.join("extensions/beta.js"),
            activation_dir.join("extensions/alpha.js"),
        ]
    );
    assert_eq!(alpha_content, "ordered extensions/alpha.js\n");
    assert_eq!(beta_content, "ordered extensions/beta.js\n");
}

async fn prepared_sidecar_plugin(
    tools: &[ToolInfo],
    hooks: &[HookRegistration],
) -> (PluginHost, PreparedPlugin) {
    let declaration = json!({
        "protocol_version": 1,
        "plugin": {"id": "test", "version": "1", "kind": "bun"},
        "tools": tools, "hooks": hooks,
    });
    let script = format!(
        r#"
import json, sys
declaration = json.loads({declaration:?})
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    result = declaration if req['method'] == 'initialize' else {{}}
    print(json.dumps({{'jsonrpc':'2.0', 'id':req['id'], 'result':result}}), flush=True)
    if req['method'] == 'shutdown': break
"#,
        declaration = declaration.to_string()
    );
    let host = PluginHost::connect(
        PluginSpec {
            id: "test".into(),
            kind: PluginKindWire::Bun,
            command: vec!["python3".into(), "-c".into(), script],
            timeout_ms: None,
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: ProcessLaunch::Bundle {
                root: std::env::temp_dir(),
                activation: Some(ActivationMetadata {
                    activation_id: "test".into(),
                    lifecycle: ActivationLifecycle::Transient,
                }),
            },
        },
        host_info(),
        None,
    )
    .await
    .unwrap();
    let plugin = host.prepared_plugins().remove(0);
    (host, plugin)
}

#[tokio::test]
async fn bundle_sidecar_tool_declaration_binds_canonical_captured_resource() {
    let _env = StableEnvGuard::new();
    let old_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("old")]))
            .expect("old fixture catalog"),
    ));
    let mut new_bundle = materialized_bundle("new");
    new_bundle.tools.clear();
    let new_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([new_bundle])).expect("new fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
    let turn_dir = tempdir();
    let old_binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture old turn binding");
    runtime
        .publish_catalog(new_catalog)
        .expect("publish replacement catalog");

    let declaration = ToolInfo {
        name: "echo".to_string(),
        description: "sidecar echo".to_string(),
        input_schema: json!({"type": "object"}),
    };
    let (host, plugin) = prepared_sidecar_plugin(&[declaration], &[]).await;
    let bindings = super::bind_bundle_sidecar_tools(&old_binding, "worker", &plugin)
        .expect("bind sidecar tool declaration");

    assert_eq!(bindings.len(), 1);
    let resolved = &bindings[0];
    assert_eq!(resolved.tool.name(), "bundle:hya/materialized/tool/echo");
    let schema = resolved.tool.schema();
    assert_eq!(schema.name.as_str(), "bundle:hya/materialized/tool/echo");
    assert_eq!(schema.description, "sidecar echo");
    assert_eq!(schema.input_schema, json!({"type": "object"}));
    assert_eq!(resolved.permission, ToolPermission::Tool);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bundle_sidecar_tool_declarations_match_loaded_module_set() {
    let _env = StableEnvGuard::new();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([disjoint_materialized_bundle(
            "selected-tools",
        )]))
        .expect("selected-tools fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture selected-tools turn binding");
    let echo = ToolInfo {
        name: "echo".to_string(),
        description: "selected echo".to_string(),
        input_schema: json!({"type": "object"}),
    };
    let (empty_host, empty) = prepared_sidecar_plugin(&[], &[]).await;
    assert!(matches!(
        super::bind_bundle_sidecar_tools(&binding, "alpha", &empty),
        Err(CoreError::Invalid(_))
    ));
    empty_host.shutdown().await.unwrap();
    let (host, plugin) = prepared_sidecar_plugin(&[echo], &[]).await;
    let bindings = super::bind_bundle_sidecar_tools(&binding, "alpha", &plugin)
        .expect("selected tool declaration must bind");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].tool.name(), "bundle:hya/materialized/tool/echo");
    host.shutdown().await.unwrap();

    let declarations = [
        ToolInfo {
            name: "echo".to_string(),
            description: "selected echo".to_string(),
            input_schema: json!({"type": "object"}),
        },
        ToolInfo {
            name: "beta".to_string(),
            description: "unselected beta".to_string(),
            input_schema: json!({"type": "object"}),
        },
    ];
    let (host, plugin) = prepared_sidecar_plugin(&declarations, &[]).await;
    assert!(matches!(
        super::bind_bundle_sidecar_tools(&binding, "alpha", &plugin),
        Err(CoreError::Invalid(_))
    ));
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bundle_sidecar_initialize_declarations_ignore_tool_and_hook_order() {
    let _env = StableEnvGuard::new();
    let mut bundle = disjoint_materialized_bundle("order-independent");
    let selected_tool_ids = bundle
        .tools
        .iter()
        .map(|resource| resource.stable_id.clone())
        .collect::<Vec<_>>();
    let selected_hook_ids = bundle
        .hooks
        .iter()
        .map(|resource| resource.stable_id.clone())
        .collect::<Vec<_>>();
    bundle.agents[0].resource_view.allow = selected_tool_ids;
    bundle.agents[0].hook_refs = selected_hook_ids;

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle]))
            .expect("order-independent declaration fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture order-independent declaration turn binding");

    let declarations = [
        ToolInfo {
            name: "echo".to_string(),
            description: "selected echo".to_string(),
            input_schema: json!({"type": "object"}),
        },
        ToolInfo {
            name: "beta".to_string(),
            description: "selected beta".to_string(),
            input_schema: json!({"type": "object"}),
        },
    ];
    let registrations = [
        HookRegistration {
            name: HookName::ToolExecuteBefore,
        },
        HookRegistration {
            name: HookName::Event,
        },
    ];

    assert!(validate_bundle_sidecar_hooks(&binding, "alpha", &registrations).is_ok());

    let (host, plugin) = prepared_sidecar_plugin(&declarations, &registrations).await;
    let bindings = bind_bundle_sidecar_tools(&binding, "alpha", &plugin)
        .expect("reverse-order tool declarations must bind");
    let canonical_names = bindings
        .iter()
        .map(|resolved| resolved.tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        canonical_names,
        vec![
            "bundle:hya/materialized/tool/beta".to_string(),
            "bundle:hya/materialized/tool/echo".to_string(),
        ]
    );
    host.shutdown().await.unwrap();

    std::fs::remove_dir_all(turn_dir).expect("cleanup order-independent turn directory");
}

#[tokio::test]
async fn bundle_sidecar_hook_only_declaration_accepts_zero_tools() {
    let _env = StableEnvGuard::new();
    let mut bundle = materialized_bundle("hook-only");
    bundle.tools.clear();
    bundle.agents[0].resource_view = ResourceView::default();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([bundle])).expect("hook-only fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture hook-only turn binding");
    let registrations = [HookRegistration {
        name: HookName::Event,
    }];
    assert!(super::validate_bundle_sidecar_hooks(&binding, "worker", &registrations).is_ok());

    let (host, plugin) = prepared_sidecar_plugin(&[], &registrations).await;
    let bindings = super::bind_bundle_sidecar_tools(&binding, "worker", &plugin)
        .expect("hook-only activation accepts zero tool declarations");
    assert!(bindings.is_empty());
    host.shutdown().await.unwrap();

    std::fs::remove_dir_all(turn_dir).expect("cleanup hook-only turn directory");
}

#[tokio::test]
async fn bundle_sidecar_tool_cancellation_returns_without_rpc_reply() {
    let _env = StableEnvGuard::new();
    let session = SessionId::new();
    let call = hya_proto::ToolCallId::new();
    let canonical_name = "bundle:hya/materialized/tool/echo".to_string();
    let input = json!({"text": "cancel"});
    let expected_input = input.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        r#"
import json, sys, socket
for line in sys.stdin:
    req = json.loads(line)
    if req['method'] == 'initialize':
        result = {{'protocol_version':1,'plugin':{{'id':'test','version':'1','kind':'bun'}},
                  'tools':[{{'name':'echo','description':'echo','inputSchema':{{'type':'object'}}}}]}}
        print(json.dumps({{'jsonrpc':'2.0','id':req['id'],'result':result}}), flush=True)
    elif req['method'] == 'tool/call':
        with socket.create_connection(('127.0.0.1', {port})) as conn:
            conn.sendall((json.dumps(req)+'\n').encode())
        sys.stdin.read()
"#
    );
    let host = PluginHost::connect(
        PluginSpec {
            id: "test".into(),
            kind: PluginKindWire::Bun,
            command: vec!["python3".into(), "-c".into(), script],
            timeout_ms: None,
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: ProcessLaunch::Bundle {
                root: std::env::temp_dir(),
                activation: Some(ActivationMetadata {
                    activation_id: "cancel".into(),
                    lifecycle: ActivationLifecycle::Transient,
                }),
            },
        },
        host_info(),
        None,
    )
    .await
    .unwrap();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (server_io, _) = listener.accept().await.unwrap();
        let mut lines = tokio::io::BufReader::new(server_io).lines();
        let line = lines
            .next_line()
            .await
            .expect("read Bundle sidecar tool request")
            .expect("Bundle sidecar tool request must be present");
        let request = match Frame::parse(&line).expect("parse Bundle sidecar request") {
            Frame::Request(request) => request,
            frame => panic!("Bundle sidecar tool call must be a request: {frame:?}"),
        };
        assert_eq!(request.method, METHOD_TOOL_CALL);
        let params: ToolCallParams =
            serde_json::from_value(request.params).expect("decode Bundle sidecar params");
        assert_eq!(params.tool, "echo");
        assert_eq!(params.session, session);
        assert_eq!(params.call, call);
        assert_eq!(params.input, expected_input);
        let _ = observed_tx.send(());
    });

    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical_name.clone(),
        Mode::Allow,
    )]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    let ctx = ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: ToolOperation::from_tool_call(call),
        mailbox: MailboxPlane::disconnected(),
        lifecycle: LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: Default::default(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    };
    let cancel = ctx.cancel.clone();
    let tool = host.prepared_plugins()[0]
        .tool("echo", &canonical_name)
        .unwrap();
    let mut execute = Box::pin(tool.execute(&ctx, input));
    let outcome = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            tokio::select! {
                result = &mut execute => (false, result),
                observed = observed_rx => {
                    if observed.is_ok() {
                        cancel.cancel();
                        (true, execute.await)
                    } else {
                        (false, Err(ToolError::Other("server did not observe Bundle sidecar tool request".to_string())))
                    }
                }
            }
        })
        .await;

    server.abort();
    let _ = server.await;

    let outcome = outcome.expect("Bundle sidecar tool must stop after cancellation");
    assert!(matches!(outcome, (true, Err(ToolError::Cancelled))));
    host.terminate().await.unwrap();
}

#[test]
fn sidecar_factory_is_scoped_to_the_selected_agent_effective_capability() {
    let _env = StableEnvGuard::new();
    // Two bundles: `worker` selects the bundle tool, `lead` denies it. The
    // sidecar factory follows each agent's own effective capability.
    let worker_bundle = materialized_bundle("selected-agent");

    let mut lead_bundle = worker_bundle.clone();
    lead_bundle.identity.id = "hya/selected-agent-lead".to_string();
    lead_bundle.digest = "selected-agent-lead-digest".to_string();
    lead_bundle.tools.clear();
    lead_bundle.hooks.clear();
    lead_bundle.extensions.clear();
    lead_bundle.agents[0] = PreparedAgent {
        id: AgentName::new("lead"),
        description: None,
        role: AgentRole::Main,
        color: None,
        prompt: Some("selected-agent lead prompt".to_string()),
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        resource_view: ResourceView::default(),
        can_spawn: vec![AgentName::new("worker")],
        hook_refs: Vec::new(),
        advertise: Vec::new(),
    };

    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([lead_bundle, worker_bundle]))
            .expect("selected-agent fixture catalog"),
    ));
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let turn_dir = tempdir();
    let binding = runtime
        .bind_turn(&turn_dir)
        .expect("capture selected-agent turn binding");
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        staging_root.clone(),
    );

    let lead_factory = environment
        .factory_for(&binding, "lead")
        .expect("resolve lead sidecar capability");
    let worker_factory = environment
        .factory_for(&binding, "worker")
        .expect("resolve worker sidecar capability");
    let lead_is_static = lead_factory.is_none();
    let worker_is_executable = worker_factory.is_some();

    std::fs::remove_dir_all(turn_dir).expect("cleanup selected-agent turn directory");
    std::fs::remove_dir_all(staging_root).expect("cleanup selected-agent staging root");

    assert!(
        lead_is_static,
        "lead must stay process-free when its effective view denies bundle echo"
    );
    assert!(
        worker_is_executable,
        "worker must retain the executable sidecar capability"
    );
}

#[test]
fn executable_bundle_builds_bound_sidecar_factory_while_static_bundle_stays_process_free() {
    let _env = StableEnvGuard::new();
    let executable_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("executable")]))
            .expect("executable fixture catalog"),
    ));
    let executable_runtime = RuntimeRegistry::new(ToolRegistry::builtins(), executable_catalog);
    let executable_turn_dir = tempdir();
    let executable_binding = executable_runtime
        .bind_turn(&executable_turn_dir)
        .expect("capture executable turn binding");
    let executable_staging = tempdir();
    let executable_environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        executable_staging.clone(),
    );
    let executable_factory = executable_environment
        .factory_for(&executable_binding, "worker")
        .expect("resolve executable sidecar factory");
    assert!(
        executable_factory.is_some(),
        "bundle extension must expose an activation factory"
    );

    let mut static_bundle = materialized_bundle("static");
    static_bundle.extensions.clear();
    static_bundle.tools.clear();
    static_bundle.hooks.clear();
    static_bundle.agents[0].hook_refs.clear();
    static_bundle.agents[0].resource_view = ResourceView::default();
    let static_catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([static_bundle]))
            .expect("static fixture catalog"),
    ));
    let static_runtime = RuntimeRegistry::new(ToolRegistry::builtins(), static_catalog);
    let static_turn_dir = tempdir();
    let static_binding = static_runtime
        .bind_turn(&static_turn_dir)
        .expect("capture static turn binding");
    let static_staging = tempdir();
    let static_environment = BundleSidecarEnvironment::from_command(
        vec!["bun".to_string(), "sidecar.js".to_string()],
        static_staging.clone(),
    );
    let static_factory = static_environment
        .factory_for(&static_binding, "worker")
        .expect("resolve static sidecar factory");
    assert!(
        static_factory.is_none(),
        "static-only bundle must remain process-free"
    );

    std::fs::remove_dir_all(executable_turn_dir).expect("cleanup executable turn directory");
    std::fs::remove_dir_all(executable_staging).expect("cleanup executable staging");
    std::fs::remove_dir_all(static_turn_dir).expect("cleanup static turn directory");
    std::fs::remove_dir_all(static_staging).expect("cleanup static staging");
}

#[test]
fn category_model_fallbacks_seeds_forward_suffix_chains() {
    let mut entries = HashMap::new();
    entries.insert(
        "deep".to_string(),
        CategoryEntry::from_candidates(&[
            "provider/first".to_string(),
            "provider/second".to_string(),
            "provider/third".to_string(),
        ])
        .expect("non-empty candidates"),
    );
    entries.insert(
        "single".to_string(),
        CategoryEntry::from_candidates(&["provider/only".to_string()])
            .expect("non-empty candidates"),
    );
    let fallbacks = category_model_fallbacks(&CategoryRegistry::from_entries(entries));

    assert_eq!(fallbacks.len(), 2, "mid-chain pick and preference only");
    assert_eq!(
        fallbacks
            .get(&ModelRef::new("provider/first"))
            .map(Vec::as_slice),
        Some(
            ["provider/first", "provider/second", "provider/third"]
                .map(ModelRef::new)
                .as_slice()
        )
    );
    assert_eq!(
        fallbacks
            .get(&ModelRef::new("provider/second"))
            .map(Vec::as_slice),
        Some(
            ["provider/second", "provider/third"]
                .map(ModelRef::new)
                .as_slice()
        ),
        "servability picks mid-chain keep forward failover"
    );
    assert!(!fallbacks.contains_key(&ModelRef::new("provider/only")));
    assert_eq!(
        category_model_fallbacks(&CategoryRegistry::default()).len(),
        0
    );
}

/// The periodic sweep drops idle Project scopes without any bind, and
/// stops with the lifecycle token.
#[tokio::test]
async fn catalog_scope_sweeper_evicts_idle_scopes_until_stopped() {
    let _env = StableEnvGuard::new();
    let (router, _model) = offline_router(None);
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        builtin_agent_catalog().unwrap(),
    ));
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(router),
            Arc::clone(&runtime),
            permission,
            EventBus::default(),
        )
        .with_catalog_scope_cache(hya_core::CatalogScopeCacheConfig {
            max_scopes: 32,
            idle_ttl: std::time::Duration::ZERO,
        }),
    );
    let root = std::env::temp_dir();
    let scope = hya_core::CatalogScope::Project {
        id: hya_proto::ProjectId::new(),
        roots: vec![root.clone()],
    };
    let binding = engine.bind_scope_runtime(&scope, &root).await.unwrap();
    runtime
        .publish_scope(
            scope.key(),
            hya_core::ScopeOverlay::new(builtin_agent_catalog().unwrap()),
        )
        .unwrap();
    drop(binding);
    assert!(runtime.scope_overlay(&scope.key()).is_some());

    let stop = tokio_util::sync::CancellationToken::new();
    let sweeper = tokio::spawn(run_catalog_scope_sweeper(
        Arc::downgrade(&engine),
        stop.clone(),
        std::time::Duration::from_millis(10),
    ));
    let mut evicted = false;
    for _ in 0..200 {
        if runtime.scope_overlay(&scope.key()).is_none() {
            evicted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(evicted, "the sweeper must evict the idle scope");
    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), sweeper)
        .await
        .expect("the sweeper stops with its token")
        .unwrap();
}

/// Test-only notification at the adapter's completed termination boundary.
pub(super) struct NotifySidecarHandle {
    pub(super) inner: Box<dyn SidecarHandle>,
    pub(super) notify: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl SidecarHandle for NotifySidecarHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.inner.ready().await
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.inner.shutdown().await
    }
    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        self.inner.loss_token()
    }
    async fn terminate(&mut self) -> Result<(), CoreError> {
        let result = self.inner.terminate().await;
        self.notify.notify_one();
        result
    }
    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.inner.tool_bindings()
    }
    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_core::hooks::HookDispatcher>> {
        self.inner.hook_dispatcher()
    }
}

#[tokio::test]
async fn each_engine_build_owns_the_store_until_shutdown_and_recovers_on_rebuild() {
    let _env = StableEnvGuard::new();
    let store = SessionStore::connect_memory().await.unwrap();
    let build = || {
        let store = store.clone();
        async move {
            let (router, model) = offline_router(None);
            let agent = agent_with_model(&model, None);
            build_session_engine_pure(
                store,
                router,
                &agent,
                BTreeMap::new(),
                Vec::new(),
                (
                    WebSearchConfig::default(),
                    InvocationPolicy::default(),
                    config::EngineSettings::default(),
                ),
            )
            .await
        }
    };
    let mut first = build().await.unwrap();
    let first_owner = first.engine().runtime_owner();
    let error = match build().await {
        Ok(mut second) => {
            second.shutdown().await.unwrap();
            first.shutdown().await.unwrap();
            panic!("a second live engine was accepted on the same store");
        }
        Err(error) => error,
    };
    assert!(
        matches!(
            error.downcast_ref::<hya_store::StoreError>(),
            Some(hya_store::StoreError::RuntimeOwnerBusy)
        ),
        "{error:#}"
    );
    let session = SessionId::new();
    let run = hya_proto::WorkflowRunId::new();
    store
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: WorkflowIdentity {
                    source: hya_proto::WorkflowSourceId::new("test:in-process-rebuild"),
                    name: "rebuild".into(),
                    revision: WorkflowRevision::from_bytes([1; 32]),
                },
                request_hash: "inputs".into(),
                owner: first_owner,
                stages: Vec::new(),
            },
        )
        .await
        .unwrap();
    first.shutdown().await.unwrap();
    let mut next = build().await.unwrap();
    first.shutdown().await.unwrap();
    assert!(matches!(
        store.claim_runtime_owner(OwnerRunId::new()),
        Err(hya_store::StoreError::RuntimeOwnerBusy)
    ));
    assert_ne!(next.engine().runtime_owner(), first_owner);
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(
        projection
            .session
            .workflow
            .as_ref()
            .unwrap()
            .run
            .as_ref()
            .unwrap()
            .status,
        hya_proto::WorkflowRunStatus::Interrupted
    );
    next.shutdown().await.unwrap();
}
