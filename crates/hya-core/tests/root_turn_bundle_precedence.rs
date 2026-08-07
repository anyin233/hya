#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Root/main turn Bundle precedence for Commit 2.
//!
//! The stable agent definition for an existing root session is taken exactly
//! from the turn's captured TurnBinding catalog. Bundle model/category defaults
//! and prepared workdirs are not per-turn overrides; session model/workdir win.

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedBundle,
    ResourceView, SpawnLifecycle,
};
use hya_core::{
    AgentCatalog, AgentSpec, BoundSidecarFactory, ChatParamsInput, ChatParamsOutcome,
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, CoreError, CreateSession, EventBus,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, RuntimeRegistry,
    SessionEngine, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, TurnBinding,
};
use hya_proto::{AgentName, ConfigGeneration, Event, FinishReason, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, ResolvedTool, Rule, ToolRegistry};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

static ENV_LOCK: AtomicBool = AtomicBool::new(false);

struct HomeGuard {
    previous: Option<OsString>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        while ENV_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
        ENV_LOCK.store(false, Ordering::Release);
    }
}

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

struct RootLengthProvider;

#[async_trait]
impl Provider for RootLengthProvider {
    fn id(&self) -> &str {
        "root-length"
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
        _request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Length,
                tokens: None,
            },
        )])))
    }
}

struct RootInFlightLossProvider {
    requests: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Provider for RootInFlightLossProvider {
    fn id(&self) -> &str {
        "capture-in-flight-loss"
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
        _request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let release = self.release.clone();
        Ok(Box::pin(futures::stream::once(async move {
            release.notified().await;
            Ok(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            })
        })))
    }
}

struct RootSidecarEnvironment {
    resolver_calls: Arc<Mutex<Vec<(ConfigGeneration, String)>>>,
    factory: Arc<RootAckGateFactory>,
}

impl SidecarEnvironment for RootSidecarEnvironment {
    fn factory_for(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.resolver_calls
            .lock()
            .unwrap()
            .push((binding.generation(), stable_id.to_string()));
        Ok(Some(self.factory.clone()))
    }
}

struct RootAckGateFactory {
    starts: Arc<Mutex<Vec<SidecarStart>>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    tool_bindings: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    shutdown_failure: Option<String>,
}

struct RootAckGateHandle {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    tool_bindings: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    shutdown_failure: Option<String>,
}

#[async_trait]
impl BoundSidecarFactory for RootAckGateFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts.lock().unwrap().push(start);
        Ok(Box::new(RootAckGateHandle {
            entered: self.entered.clone(),
            release: self.release.clone(),
            tool_bindings: self.tool_bindings.clone(),
            shutdowns: self.shutdowns.clone(),
            terminates: self.terminates.clone(),
            shutdown_failure: self.shutdown_failure.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for RootAckGateHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = &self.shutdown_failure {
            return Err(CoreError::Invalid(message.clone()));
        }
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.tool_bindings.fetch_add(1, Ordering::SeqCst);
        Arc::from([])
    }
}

struct RootPendingStartEnvironment {
    resolver_calls: Arc<AtomicUsize>,
    factory: Arc<RootPendingStartFactory>,
}

impl SidecarEnvironment for RootPendingStartEnvironment {
    fn factory_for(
        &self,
        _binding: &TurnBinding,
        _stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.resolver_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.factory.clone()))
    }
}

struct RootPendingStartFactory {
    starts: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    handle_calls: Arc<AtomicUsize>,
}

struct RootPendingStartHandle {
    handle_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for RootPendingStartFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(Box::new(RootPendingStartHandle {
            handle_calls: self.handle_calls.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for RootPendingStartHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        Arc::from([])
    }
}

struct RootLossEnvironment {
    resolver_calls: Arc<AtomicUsize>,
    factory: Arc<RootLossFactory>,
}

impl SidecarEnvironment for RootLossEnvironment {
    fn factory_for(
        &self,
        _binding: &TurnBinding,
        _stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.resolver_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.factory.clone()))
    }
}

struct RootLossFactory {
    starts: Arc<Mutex<Vec<SidecarStart>>>,
    transport_loss: CancellationToken,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
}

struct RootLossHandle {
    transport_loss: CancellationToken,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for RootLossFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts.lock().unwrap().push(start);
        Ok(Box::new(RootLossHandle {
            transport_loss: self.transport_loss.clone(),
            shutdowns: self.shutdowns.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for RootLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.transport_loss.cancel();
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.transport_loss.clone())
    }
}

struct RootInFlightLossEnvironment {
    resolver_calls: Arc<AtomicUsize>,
    factory: Arc<RootInFlightLossFactory>,
}

impl SidecarEnvironment for RootInFlightLossEnvironment {
    fn factory_for(
        &self,
        _binding: &TurnBinding,
        _stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.resolver_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.factory.clone()))
    }
}

struct RootInFlightLossFactory {
    starts: Arc<Mutex<Vec<SidecarStart>>>,
    transport_loss: CancellationToken,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
}

struct RootInFlightLossHandle {
    transport_loss: CancellationToken,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for RootInFlightLossFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts.lock().unwrap().push(start);
        Ok(Box::new(RootInFlightLossHandle {
            transport_loss: self.transport_loss.clone(),
            shutdowns: self.shutdowns.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for RootInFlightLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.transport_loss.clone())
    }
}

struct RootActivationHookProbe {
    events: Arc<Mutex<Vec<hya_proto::Envelope>>>,
    healthy: Arc<AtomicBool>,
    unhealthy_event_calls: Option<Arc<AtomicUsize>>,
    before_calls: Option<Arc<AtomicUsize>>,
    after_calls: Option<Arc<AtomicUsize>>,
}

#[async_trait]
impl HookDispatcher for RootActivationHookProbe {
    fn dispatch_event(&self, envelope: &hya_proto::Envelope) {
        self.events.lock().unwrap().push(envelope.clone());
        if let Some(unhealthy_event_calls) = &self.unhealthy_event_calls {
            unhealthy_event_calls.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        if let Some(before_calls) = &self.before_calls {
            before_calls.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        if let Some(after_calls) = &self.after_calls {
            after_calls.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

struct RootActivationHookEnvironment {
    resolver_calls: Arc<AtomicUsize>,
    factory: Arc<RootActivationHookFactory>,
}

impl SidecarEnvironment for RootActivationHookEnvironment {
    fn factory_for(
        &self,
        _binding: &TurnBinding,
        _stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.resolver_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.factory.clone()))
    }
}

struct RootActivationHookFactory {
    starts: Arc<Mutex<Vec<SidecarStart>>>,
    hook_accessors: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    hooks: Arc<dyn HookDispatcher>,
}

struct RootActivationHookHandle {
    hook_accessors: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    hooks: Arc<dyn HookDispatcher>,
}

#[async_trait]
impl BoundSidecarFactory for RootActivationHookFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts.lock().unwrap().push(start);
        Ok(Box::new(RootActivationHookHandle {
            hook_accessors: self.hook_accessors.clone(),
            shutdowns: self.shutdowns.clone(),
            terminates: self.terminates.clone(),
            hooks: self.hooks.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for RootActivationHookHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        self.hook_accessors.fetch_add(1, Ordering::SeqCst);
        Some(self.hooks.clone())
    }
}

#[derive(Clone)]
struct AgentFixture {
    stable_id: String,
    prompt: Option<String>,
    model: Option<String>,
    category: Option<String>,
    reasoning: Option<String>,
    workdir: Option<String>,
}

impl AgentFixture {
    fn new(stable_id: impl Into<String>) -> Self {
        Self {
            stable_id: stable_id.into(),
            prompt: None,
            model: None,
            category: None,
            reasoning: None,
            workdir: None,
        }
    }

    fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    fn reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    fn workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = Some(workdir.into());
        self
    }
}

fn catalog(agents: &[AgentFixture]) -> Arc<AgentCatalog> {
    // A bundle defines one agent, so each fixture agent becomes its own bundle.
    let bundles = agents
        .iter()
        // Built-in ids are compiled in; a fixture asking for one just gets it.
        .filter(|agent| !hya_core::is_builtin_id(&agent.stable_id))
        .map(|agent| PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: format!("hya/root-turn-precedence-{}", agent.stable_id),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("test-only-{}", agent.stable_id),
            agent: PreparedAgent {
                id: AgentName::new(&agent.stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: agent.prompt.clone(),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy {
                    model: agent.model.clone(),
                    category: agent.category.clone(),
                    reasoning: agent.reasoning.clone(),
                },
                workdir: agent.workdir.clone(),
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

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} body\n"),
    )
    .unwrap();
}

async fn engine_with(catalog: Arc<AgentCatalog>, provider: Arc<CaptureProvider>) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog,
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

async fn engine_with_provider(
    catalog: Arc<AgentCatalog>,
    provider: Arc<dyn Provider>,
) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog,
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

const BASE_MARKER: &str = "ROOT_TURN_BASE_PROMPT_MARKER";
const AGENTS_MARKER: &str = "ROOT_TURN_AGENTS_CONTEXT_MARKER";
const BUNDLE_PROMPT: &str = "EXACT_BUNDLE_PROMPT_FOR_EXPLORE";

fn composed_base(workdir: PathBuf) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("base-caller"),
        model: ModelRef::new("base-model"),
        system_prompt: [
            BASE_MARKER,
            "You are the composed base agent.",
            "",
            "## Project context: AGENTS.md",
            AGENTS_MARKER,
            "",
            "## Environment",
            "- cwd: /composed",
        ]
        .join("\n"),
        workdir,
        reasoning: Some(ReasoningEffort::Low),
    }
}

fn skill_header_count(system: &str) -> usize {
    system
        .matches("These skills are available on demand; read the named SKILL.md when relevant:")
        .count()
}

#[tokio::test]
async fn root_turn_missing_definition_fails_closed_without_general_fallback() {
    let home = support::TestDir::new("root-missing-home");
    let workdir = support::TestDir::new("root-missing-def");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("general")
            .model("bundle-general-model")
            .category("quick")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("ghost"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "should not run".to_string())
        .await
        .unwrap();

    let err = engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing root definition must fail closed");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not synthesize or fall back to general for a missing root definition"
    );
}

#[tokio::test]
async fn root_turn_prompt_none_preserves_composed_base_and_appends_skills_once() {
    let home = support::TestDir::new("root-prompt-none-home");
    let workdir = support::TestDir::new("root-prompt-none");
    let _home = HomeGuard::set(home.path());
    write_skill(workdir.path(), "session-skill");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("build")
            .model("bundle-default-model")
            .category("deep")
            .workdir("/bundle/must-not-win")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "preserve base".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(PathBuf::from("/agent/base")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.contains(BASE_MARKER),
        "prompt=None must preserve the composed base prompt: {system}"
    );
    assert!(
        system.contains(AGENTS_MARKER),
        "prompt=None must not erase AGENTS/context composition: {system}"
    );
    assert_eq!(skill_header_count(system), 1, "skill section once");
    assert!(system.contains("session-skill"));
    assert!(
        system.find(BASE_MARKER).unwrap()
            < system.find("These skills are available on demand").unwrap(),
        "skills append after preserved base"
    );
    assert!(
        !system.contains(BUNDLE_PROMPT),
        "prompt=None must not inject a Bundle prompt body"
    );
    assert_eq!(requests[0].model.as_str(), "session-model");
}

#[tokio::test]
async fn root_turn_bundle_prompt_replaces_base_and_sees_no_workdir_skill() {
    let home = support::TestDir::new("root-prompt-replace-home");
    let workdir = support::TestDir::new("root-prompt-replace");
    let _home = HomeGuard::set(home.path());
    write_skill(workdir.path(), "session-skill");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("bundle-explorer").prompt(BUNDLE_PROMPT)]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("bundle-explorer"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "replace base".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.starts_with(BUNDLE_PROMPT),
        "non-empty Bundle prompt replaces the base once: {system}"
    );
    assert!(
        !system.contains(BASE_MARKER),
        "replaced base must not remain: {system}"
    );
    assert!(
        !system.contains(AGENTS_MARKER),
        "replaced base composition must not remain: {system}"
    );
    // A bundle agent is on the clamped plane, so workdir-discovered skills are
    // not part of its view and never reach its prompt.
    assert_eq!(skill_header_count(system), 0);
    assert!(
        !system.contains("session-skill"),
        "a bundle agent must not see a project skill: {system}"
    );
    assert_eq!(
        system.matches(BUNDLE_PROMPT).count(),
        1,
        "bundle prompt applied exactly once"
    );
}

#[tokio::test]
async fn root_turn_session_model_and_model_switched_win_over_base_and_bundle_defaults() {
    let home = support::TestDir::new("root-session-model-home");
    let workdir = support::TestDir::new("root-session-model");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("build")
            .model("bundle-default-model")
            .category("ultrabrain")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("created-session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use session model".to_string())
        .await
        .unwrap();

    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests[0].model.as_str(), "created-session-model");
        assert_ne!(requests[0].model.as_str(), "base-model");
        assert_ne!(requests[0].model.as_str(), "bundle-default-model");
    }

    engine
        .switch_model(session, ModelRef::new("switched-model"))
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use switched model".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].model.as_str(), "switched-model");
    assert!(
        !requests.iter().any(|req| {
            req.model.as_str() == "bundle-default-model" || req.model.as_str() == "base-model"
        }),
        "Bundle model/category and base AgentSpec must not override persisted session model"
    );
}

#[tokio::test]
async fn root_turn_bundle_reasoning_override_and_absent_preserves_base() {
    let home = support::TestDir::new("root-reasoning-home");
    let workdir = support::TestDir::new("root-reasoning");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[
            AgentFixture::new("with-reasoning").reasoning("high"),
            AgentFixture::new("no-reasoning"),
        ]),
        provider.clone(),
    )
    .await;

    let override_session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("with-reasoning"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(override_session, "override reasoning".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            override_session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let preserve_session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("no-reasoning"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(preserve_session, "preserve reasoning".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            preserve_session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].reasoning, Some(ReasoningEffort::High));
    assert_eq!(
        requests[1].reasoning,
        Some(ReasoningEffort::Low),
        "absent Bundle reasoning must preserve the base AgentSpec reasoning"
    );
}

#[tokio::test]
async fn root_turn_session_workdir_wins_over_bundle_and_base_workdir() {
    let home = support::TestDir::new("root-workdir-home");
    let session_dir = support::TestDir::new("root-session-workdir");
    let bundle_dir = support::TestDir::new("root-bundle-workdir");
    let agent_dir = support::TestDir::new("root-agent-workdir");
    let _home = HomeGuard::set(home.path());
    write_skill(session_dir.path(), "from-session");
    write_skill(bundle_dir.path(), "from-bundle");
    write_skill(agent_dir.path(), "from-agent");

    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[
            AgentFixture::new("build").workdir(bundle_dir.path().to_string_lossy().into_owned())
        ]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: session_dir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "session workdir".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(agent_dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.contains("from-session"),
        "skills must come from persisted session workdir: {system}"
    );
    assert!(
        !system.contains("from-bundle"),
        "Bundle prepared workdir must not redirect an existing root turn: {system}"
    );
    assert!(
        !system.contains("from-agent"),
        "base AgentSpec workdir must not redirect when session workdir is set: {system}"
    );
    assert_eq!(skill_header_count(system), 1);
}

#[tokio::test]
async fn root_turn_records_one_turn_binding() {
    let home = support::TestDir::new("root-one-binding-home");
    let workdir = support::TestDir::new("root-one-binding");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(catalog(&[AgentFixture::new("build")]), provider).await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "one binding".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let bindings = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| matches!(envelope.event, Event::TurnBindingRecorded { .. }))
        .count();
    assert_eq!(
        bindings, 1,
        "root turn must capture exactly one TurnBinding"
    );
}

#[tokio::test]
async fn root_sidecar_resolver_uses_captured_binding_and_acks_before_model_poll() {
    let home = support::TestDir::new("root-sidecar-home");
    let workdir = support::TestDir::new("root-sidecar");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tool_bindings = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(RootAckGateFactory {
        starts: starts.clone(),
        entered: entered.clone(),
        release: release.clone(),
        tool_bindings: tool_bindings.clone(),
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        shutdown_failure: None,
    });
    let environment = Arc::new(RootSidecarEnvironment {
        resolver_calls: resolver_calls.clone(),
        factory,
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment.clone() as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "start executable root".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root sidecar test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let first_agent = agent.clone();
    let turn_engine = engine.clone();
    let turn = tokio::spawn(async move {
        turn_engine
            .run_turn(session, &first_agent, CancellationToken::new())
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("root sidecar must enter its ready ACK gate");

    let captured_generation = {
        let calls = resolver_calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "root resolves its sidecar exactly once");
        assert_eq!(calls[0].1, "build");
        calls[0].0
    };
    let published_generation = engine
        .runtime_registry()
        // Publishing a *different* installed catalog is what advances the
        // generation; built-in ids are compiled in and never vary.
        .publish_catalog(catalog(&[
            AgentFixture::new("sidecar-generation-marker").prompt("root sidecar generation N+1")
        ]))
        .expect("publish root sidecar replacement catalog");
    assert!(
        published_generation.get() > captured_generation.get(),
        "root sidecar publication must advance the captured generation"
    );
    assert_eq!(
        tool_bindings.load(Ordering::SeqCst),
        0,
        "executable dispatch compilation must wait for the sidecar ready ACK"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "model polling must wait for the sidecar ready ACK"
    );
    assert!(
        !engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|envelope| {
                matches!(
                    envelope.event,
                    Event::MessageStarted {
                        role: Role::Assistant,
                        ..
                    }
                )
            }),
        "assistant message events must wait for the sidecar ready ACK"
    );
    {
        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 1, "root starts one transient sidecar");
        assert_eq!(starts[0].lifecycle, SidecarLifecycle::Transient);
    }

    release.notify_one();
    assert_eq!(
        turn.await.unwrap().unwrap(),
        FinishReason::Stop,
        "the model turn completes after ACK"
    );
    let binding_generation = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .find_map(|envelope| match envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .expect("root turn must persist its captured binding");
    {
        let calls = resolver_calls.lock().unwrap();
        assert_eq!(calls[0].0, binding_generation);
    }
    assert_eq!(
        tool_bindings.load(Ordering::SeqCst),
        1,
        "executable dispatch compiles exactly once after the sidecar ready ACK"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);

    engine
        .admit_user_prompt(session, "start second executable root".to_string())
        .await
        .unwrap();
    let second_engine = engine.clone();
    let second_turn = tokio::spawn(async move {
        second_engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("second root sidecar must enter its ready ACK gate");
    {
        let calls = resolver_calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "root resolves each sidecar exactly once");
        assert_eq!(calls[0].1, calls[1].1, "root stable id remains unchanged");
        assert_eq!(calls[1].1, "build");
        assert_eq!(
            calls[1].0, published_generation,
            "second root must resolve the published generation"
        );
    }
    release.notify_one();
    assert_eq!(
        second_turn.await.unwrap().unwrap(),
        FinishReason::Stop,
        "the second root turn completes after ACK"
    );
    {
        let starts = starts.lock().unwrap();
        assert_eq!(
            starts.len(),
            2,
            "root starts one transient sidecar per turn"
        );
        assert_eq!(starts[0].lifecycle, SidecarLifecycle::Transient);
        assert_eq!(starts[1].lifecycle, SidecarLifecycle::Transient);
    }
    assert_eq!(
        tool_bindings.load(Ordering::SeqCst),
        2,
        "each root turn compiles executable dispatch after its sidecar ACK"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_sidecar_length_completion_gracefully_shuts_down() {
    let home = support::TestDir::new("root-sidecar-length-home");
    let workdir = support::TestDir::new("root-sidecar-length");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(RootLengthProvider);
    let resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tool_bindings = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootSidecarEnvironment {
        resolver_calls,
        factory: Arc::new(RootAckGateFactory {
            starts,
            entered: entered.clone(),
            release: release.clone(),
            tool_bindings: tool_bindings.clone(),
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            shutdown_failure: None,
        }),
    });
    let engine = Arc::new(
        engine_with_provider(catalog(&[AgentFixture::new("build")]), provider)
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "complete by length".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root sidecar length test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let turn_engine = engine.clone();
    let turn_agent = agent.clone();
    let turn = tokio::spawn(async move {
        turn_engine
            .run_turn(session, &turn_agent, CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("root sidecar must enter its ready ACK gate");
    assert_eq!(tool_bindings.load(Ordering::SeqCst), 0);

    release.notify_one();
    assert_eq!(
        turn.await.unwrap().unwrap(),
        FinishReason::Length,
        "a length-complete model turn must finish after ACK"
    );
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_sidecar_loss_after_ack_fails_before_dispatch_and_model_poll() {
    let home = support::TestDir::new("root-sidecar-loss-home");
    let workdir = support::TestDir::new("root-sidecar-loss");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let transport_loss = CancellationToken::new();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootLossEnvironment {
        resolver_calls: resolver_calls.clone(),
        factory: Arc::new(RootLossFactory {
            starts: starts.clone(),
            transport_loss,
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
        }),
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "transport loss after ACK".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root sidecar loss test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let error = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .expect_err("transport loss after ACK must cancel before dispatch");
    assert!(matches!(error, CoreError::Cancelled));
    assert_eq!(resolver_calls.load(Ordering::SeqCst), 1);
    {
        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].lifecycle, SidecarLifecycle::Transient);
    }
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);

    let events = engine.replay(session).await.unwrap();
    assert!(!events.iter().any(|envelope| {
        matches!(
            envelope.event,
            Event::MessageStarted {
                role: Role::Assistant,
                ..
            }
        )
    }));
    assert!(
        !events
            .iter()
            .any(|envelope| { matches!(envelope.event, Event::TurnBindingRecorded { .. }) })
    );
}

#[tokio::test]
async fn root_sidecar_loss_during_model_terminates_before_released_output() {
    let home = support::TestDir::new("root-sidecar-in-flight-loss-home");
    let workdir = support::TestDir::new("root-sidecar-in-flight-loss");
    let _home = HomeGuard::set(home.path());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let requests = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(RootInFlightLossProvider {
        requests: requests.clone(),
        entered: entered.clone(),
        release: release.clone(),
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let transport_loss = CancellationToken::new();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootInFlightLossEnvironment {
        resolver_calls: resolver_calls.clone(),
        factory: Arc::new(RootInFlightLossFactory {
            starts: starts.clone(),
            transport_loss: transport_loss.clone(),
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
        }),
    });
    let engine = Arc::new(
        engine_with_provider(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "transport loss during model".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root in-flight loss test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let turn_engine = engine.clone();
    let turn = tokio::spawn(async move {
        turn_engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
    });
    entered.notified().await;
    transport_loss.cancel();
    release.notify_one();

    let error = turn
        .await
        .unwrap()
        .expect_err("transport loss during model streaming must cancel the turn");
    assert!(matches!(error, CoreError::Cancelled));
    assert_eq!(resolver_calls.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    {
        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].lifecycle, SidecarLifecycle::Transient);
    }
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);

    let events = engine.replay(session).await.unwrap();
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )
    }));
    assert_eq!(
        events
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MessageFinished {
                        role: Role::Assistant,
                        finish: FinishReason::Cancelled,
                        ..
                    }
                )
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn root_sidecar_activation_dispatcher_observes_post_ack_turn_events() {
    let home = support::TestDir::new("root-sidecar-hooks-home");
    let workdir = support::TestDir::new("root-sidecar-hooks");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let observed_events = Arc::new(Mutex::new(Vec::new()));
    let hooks = Arc::new(RootActivationHookProbe {
        events: observed_events.clone(),
        healthy: Arc::new(AtomicBool::new(true)),
        unhealthy_event_calls: None,
        before_calls: None,
        after_calls: None,
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let hook_accessors = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootActivationHookEnvironment {
        resolver_calls: resolver_calls.clone(),
        factory: Arc::new(RootActivationHookFactory {
            starts: starts.clone(),
            hook_accessors: hook_accessors.clone(),
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            hooks: hooks.clone(),
        }),
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider)
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "observe root activation hooks".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root activation hook test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(resolver_calls.load(Ordering::SeqCst), 1);
    {
        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].lifecycle, SidecarLifecycle::Transient);
    }
    assert_eq!(hook_accessors.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);

    let observed = observed_events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::MessageStarted {
                session: event_session,
                role: Role::User,
                ..
            } if *event_session == session => Some("user_message"),
            Event::MessageStarted {
                session: event_session,
                role: Role::Assistant,
                ..
            } if *event_session == session => Some("assistant_message_started"),
            Event::TurnBindingRecorded {
                session: event_session,
                ..
            } if *event_session == session => Some("turn_binding_recorded"),
            Event::StepStarted {
                session: event_session,
                ..
            } if *event_session == session => Some("step_started"),
            Event::StepFinished {
                session: event_session,
                ..
            } if *event_session == session => Some("step_finished"),
            Event::MessageFinished {
                session: event_session,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            } if *event_session == session => Some("assistant_message_finished"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!observed.contains(&"user_message"));
    assert_eq!(
        observed,
        vec![
            "assistant_message_started",
            "turn_binding_recorded",
            "step_started",
            "step_finished",
            "assistant_message_finished",
        ]
    );
}

#[tokio::test]
async fn root_sidecar_after_hook_transport_loss_fences_tool_event_before_commit() {
    let home = support::TestDir::new("root-sidecar-after-hook-loss-home");
    let workdir = support::TestDir::new("root-sidecar-after-hook-loss");
    let _home = HomeGuard::set(home.path());
    let after_calls = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(RootActivationHookProbe {
        events: Arc::new(Mutex::new(Vec::new())),
        healthy: Arc::new(AtomicBool::new(true)),
        unhealthy_event_calls: None,
        before_calls: None,
        after_calls: Some(after_calls.clone()),
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let hook_accessors = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootActivationHookEnvironment {
        resolver_calls,
        factory: Arc::new(RootActivationHookFactory {
            starts,
            hook_accessors,
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            hooks,
        }),
    });
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]));
    let engine = Arc::new(
        engine_with_provider(catalog(&[AgentFixture::new("build")]), provider)
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "run echo after hook loss".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root activation hook loss test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert!(matches!(result, Err(CoreError::Cancelled)));
    assert_eq!(after_calls.load(Ordering::SeqCst), 1);

    let events = engine.replay(session).await.unwrap();
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { .. } | Event::ToolError { .. }
        )
    }));
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )
    }));
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_sidecar_before_hook_transport_loss_stops_before_after_hook_or_commit() {
    let home = support::TestDir::new("root-sidecar-before-hook-loss-home");
    let workdir = support::TestDir::new("root-sidecar-before-hook-loss");
    let _home = HomeGuard::set(home.path());
    let before_calls = Arc::new(AtomicUsize::new(0));
    let after_calls = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(RootActivationHookProbe {
        events: Arc::new(Mutex::new(Vec::new())),
        healthy: Arc::new(AtomicBool::new(true)),
        unhealthy_event_calls: None,
        before_calls: Some(before_calls.clone()),
        after_calls: Some(after_calls.clone()),
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let hook_accessors = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootActivationHookEnvironment {
        resolver_calls,
        factory: Arc::new(RootActivationHookFactory {
            starts,
            hook_accessors,
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            hooks,
        }),
    });
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]));
    let engine = Arc::new(
        engine_with_provider(catalog(&[AgentFixture::new("build")]), provider)
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "run echo before hook loss".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root activation hook before-loss test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert_eq!(before_calls.load(Ordering::SeqCst), 1);
    assert_eq!(after_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(result, Err(CoreError::Cancelled)));

    let events = engine.replay(session).await.unwrap();
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { .. } | Event::ToolError { .. }
        )
    }));
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )
    }));
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_sidecar_event_transport_loss_stops_before_model_poll() {
    let home = support::TestDir::new("root-sidecar-event-loss-home");
    let workdir = support::TestDir::new("root-sidecar-event-loss");
    let _home = HomeGuard::set(home.path());
    let unhealthy_event_calls = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(RootActivationHookProbe {
        events: Arc::new(Mutex::new(Vec::new())),
        healthy: Arc::new(AtomicBool::new(true)),
        unhealthy_event_calls: Some(unhealthy_event_calls.clone()),
        before_calls: None,
        after_calls: None,
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let hook_accessors = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootActivationHookEnvironment {
        resolver_calls,
        factory: Arc::new(RootActivationHookFactory {
            starts,
            hook_accessors,
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            hooks,
        }),
    });
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "event transport loss before poll".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root activation event loss test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert!(unhealthy_event_calls.load(Ordering::SeqCst) >= 1);
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(matches!(result, Err(CoreError::Cancelled)));
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);

    let events = engine.replay(session).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::StepStarted { .. }))
    );
    assert!(!events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn root_sidecar_shutdown_failure_is_not_reported_as_success() {
    let home = support::TestDir::new("root-sidecar-shutdown-failure-home");
    let workdir = support::TestDir::new("root-sidecar-shutdown-failure");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tool_bindings = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootSidecarEnvironment {
        resolver_calls,
        factory: Arc::new(RootAckGateFactory {
            starts,
            entered: entered.clone(),
            release: release.clone(),
            tool_bindings,
            shutdowns: shutdowns.clone(),
            terminates: terminates.clone(),
            shutdown_failure: Some("root sidecar shutdown failed".to_string()),
        }),
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "shutdown failure must surface".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root sidecar shutdown failure test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let turn_engine = engine.clone();
    let turn_agent = agent.clone();
    let turn = tokio::spawn(async move {
        turn_engine
            .run_turn(session, &turn_agent, CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("root sidecar must enter its ready ACK gate");
    release.notify_one();

    let result = turn.await.unwrap();
    assert!(matches!(
        result,
        Err(CoreError::Invalid(message)) if message == "root sidecar shutdown failed"
    ));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_sidecar_cancel_while_factory_start_is_pending_stops_before_model_poll() {
    let home = support::TestDir::new("root-sidecar-pending-start-home");
    let workdir = support::TestDir::new("root-sidecar-pending-start");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let handle_calls = Arc::new(AtomicUsize::new(0));
    let environment = Arc::new(RootPendingStartEnvironment {
        resolver_calls: resolver_calls.clone(),
        factory: Arc::new(RootPendingStartFactory {
            starts: starts.clone(),
            entered: entered.clone(),
            release,
            handle_calls: handle_calls.clone(),
        }),
    });
    let engine = Arc::new(
        engine_with(catalog(&[AgentFixture::new("build")]), provider.clone())
            .await
            .with_sidecar_environment(environment as Arc<dyn SidecarEnvironment>),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "cancel pending sidecar start".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: "root pending sidecar start test".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    let cancel = CancellationToken::new();
    let turn_cancel = cancel.clone();
    let turn_engine = engine.clone();
    let mut turn =
        tokio::spawn(async move { turn_engine.run_turn(session, &agent, turn_cancel).await });
    entered.notified().await;
    cancel.cancel();

    let result = match tokio::time::timeout(std::time::Duration::from_secs(1), &mut turn).await {
        Ok(joined) => joined.unwrap(),
        Err(_) => {
            turn.abort();
            let _ = turn.await;
            panic!("run_turn remained blocked while sidecar factory start was pending");
        }
    };
    assert!(matches!(result, Err(CoreError::Cancelled)));
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(resolver_calls.load(Ordering::SeqCst), 1);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(handle_calls.load(Ordering::SeqCst), 0);
}
