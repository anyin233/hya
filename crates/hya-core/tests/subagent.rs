//! Integration tests for `hya-core`: subagent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedResource,
    ResourceView,
};
use hya_core::{
    AdmissionMemberIdentity, AgentCatalog, AgentSpec, BoundSidecarFactory, BoundSpawnSender,
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    CoreError, CreateSession, EventBus, HookDispatcher, MemberSpec, MemberStatus,
    MessageUserBeforeInput, MessageUserBeforeOutcome, ResidentSupervisor, RuntimeRegistry,
    SessionEngine, SidecarHandle, SidecarLifecycle, SidecarStart, SubagentGovernor, SubagentLimits,
    TeamEvidenceEnvelope, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, project_envelope,
    run_pre_admitted_member, run_team,
};
use hya_proto::{
    AgentName, Event, FinishReason, MailEndpoint, MailKind, MemberId, MemberRunStatus, MessageId,
    ModelRef, PartProjection, Role, RosterStatus, SessionId, ToolCallId, ToolName, ToolSchema,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, MemberOutcome, Mode, PermissionPlane, PermissionRules, ResolvedTool, Resource, Rule,
    Tool, ToolCtx, ToolError, ToolOperation, ToolPermission, ToolRegistry,
};
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Barrier, Notify, mpsc};
use tokio_util::sync::CancellationToken;

struct SelectiveFakeProvider;

#[async_trait]
impl Provider for SelectiveFakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("MEMBERTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

struct AckProbeProvider {
    polls: Arc<AtomicUsize>,
    polled: Arc<Notify>,
}

#[async_trait]
impl Provider for AckProbeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.polled.notify_one();
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("ACKTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

struct ResidentStopBarrierProvider {
    polls: Arc<AtomicUsize>,
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Notify>,
}

#[async_trait]
impl Provider for ResidentStopBarrierProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.entered
            .send(())
            .map_err(|_| ProviderError::Decode("resident stop barrier dropped".to_string()))?;
        self.release.notified().await;
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("RESIDENT_STOP_TEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

struct TransientLossProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Provider for TransientLossProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Ok(Box::pin(stream::once(async move {
            // Signal only once the engine polls the blocked stream. Opening the
            // stream is too early: StepStarted still has to be persisted, and
            // cancellation there can drop the sole in-memory SQLite connection.
            entered.notify_one();
            release.notified().await;
            Ok::<Event, ProviderError>(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            })
        })))
    }
}

struct TransientLossFactory {
    healthy: Arc<AtomicBool>,
    loss: CancellationToken,
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

struct TransientLossHandle {
    healthy: Arc<AtomicBool>,
    loss: CancellationToken,
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for TransientLossFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(TransientLossHandle {
            healthy: self.healthy.clone(),
            loss: self.loss.clone(),
            terminated: self.terminated.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for TransientLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.healthy.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(false, Ordering::SeqCst);
        self.terminated.notify_one();
        Ok(())
    }
}

struct AckGateFactory {
    starts: mpsc::UnboundedSender<SidecarStart>,
    ready: Arc<Notify>,
}

#[async_trait]
impl BoundSidecarFactory for AckGateFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts
            .send(start)
            .map_err(|_| CoreError::Invalid("sidecar start receiver dropped".to_string()))?;
        Ok(Box::new(AckGateHandle {
            ready: self.ready.clone(),
        }))
    }
}

struct AckGateHandle {
    ready: Arc<Notify>,
}

#[async_trait]
impl SidecarHandle for AckGateHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.ready.notified().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

struct AckFailureFactory {
    ready: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
}

struct AckFailureHandle {
    ready: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for AckFailureFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(AckFailureHandle {
            ready: self.ready.clone(),
            terminates: self.terminates.clone(),
            shutdowns: self.shutdowns.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for AckFailureHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.ready.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Invalid("sidecar ACK rejected".to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct SidecarPermissionProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    turn: AtomicUsize,
}

#[async_trait]
impl Provider for SidecarPermissionProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
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
        let script = if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
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

struct SidecarPermissionTool {
    name: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SidecarPermissionTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "sidecar permission probe".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        ctx.permission
            .assert(Action::Tool, Resource::Tool(self.name.clone()))
            .await?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    }
}

struct ImmediateSidecarFactory {
    bindings: Arc<[ResolvedTool]>,
}

struct ImmediateSidecarHandle {
    bindings: Arc<[ResolvedTool]>,
}

const SIDECAR_PERMISSION_TOOL: &str = "bundle:hya/sidecar-permission/tool/echo";

// Episode ending: five seconds for the turn, five for its sidecar, one for commit.
const RESIDENT_END_BOUND: std::time::Duration = std::time::Duration::from_secs(11);

fn sidecar_permission_bundle() -> Bundle {
    Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/sidecar-permission".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: "test-only".to_string(),
        agents: vec![PreparedAgent {
            id: AgentName::new("sidecar-agent"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some("sidecar permission prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView::default(),
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        }],
        tools: vec![PreparedResource {
            local_id: "echo".to_string(),
            stable_id: SIDECAR_PERMISSION_TOOL.to_string(),
            source_path: "tools/echo.js".to_string(),
            digest: "test-only-tool".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        }],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,

        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    }
}

#[async_trait]
impl BoundSidecarFactory for ImmediateSidecarFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(ImmediateSidecarHandle {
            bindings: self.bindings.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ImmediateSidecarHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.bindings.clone()
    }
}

struct ResidentStopProbeFactory {
    ready: Arc<AtomicUsize>,
    healthy: Arc<AtomicBool>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    terminated: Option<Arc<Notify>>,
    loss: Option<CancellationToken>,
}

struct ResidentStopProbeHandle {
    ready: Arc<AtomicUsize>,
    healthy: Arc<AtomicBool>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    terminated: Option<Arc<Notify>>,
    loss: Option<CancellationToken>,
}

#[async_trait]
impl BoundSidecarFactory for ResidentStopProbeFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(ResidentStopProbeHandle {
            ready: self.ready.clone(),
            healthy: self.healthy.clone(),
            shutdowns: self.shutdowns.clone(),
            terminates: self.terminates.clone(),
            terminated: self.terminated.clone(),
            loss: self.loss.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ResidentStopProbeHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.ready.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        self.loss.clone()
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(false, Ordering::SeqCst);
        if let Some(terminated) = &self.terminated {
            terminated.notify_one();
        }
        Ok(())
    }
}

struct ResidentTerminateFailureFactory {
    loss: CancellationToken,
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

struct ResidentTerminateFailureHandle {
    loss: CancellationToken,
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for ResidentTerminateFailureFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(ResidentTerminateFailureHandle {
            loss: self.loss.clone(),
            terminated: self.terminated.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ResidentTerminateFailureHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.terminated.notify_one();
        Err(CoreError::Invalid(
            "test resident terminate failure".to_string(),
        ))
    }
}

struct ResidentPreworkLossFactory {
    starts: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminated: Arc<Notify>,
    first_loss: CancellationToken,
}

struct ResidentPreworkLossHandle {
    loss: Option<CancellationToken>,
    terminates: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    terminated: Arc<Notify>,
}

#[async_trait]
impl BoundSidecarFactory for ResidentPreworkLossFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(ResidentPreworkLossHandle {
            loss: (attempt == 1).then(|| self.first_loss.clone()),
            terminates: self.terminates.clone(),
            shutdowns: self.shutdowns.clone(),
            terminated: self.terminated.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ResidentPreworkLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        self.loss.clone()
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.terminated.notify_one();
        Ok(())
    }
}

struct DeclarationDriftFactory {
    starts: Arc<AtomicUsize>,
    healthy: Arc<AtomicBool>,
    terminates: Arc<AtomicUsize>,
}

struct DeclarationDriftHandle {
    healthy: Arc<AtomicBool>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for DeclarationDriftFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt != 1 {
            return Err(CoreError::Invalid(
                "bundle sidecar declaration drift".to_string(),
            ));
        }
        Ok(Box::new(DeclarationDriftHandle {
            healthy: self.healthy.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for DeclarationDriftHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.healthy.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct ActivationHookProbe {
    tool_before: Arc<AtomicUsize>,
    tool_after: Arc<AtomicUsize>,
    leak: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<hya_proto::Envelope>>>,
}

#[async_trait]
impl HookDispatcher for ActivationHookProbe {
    fn dispatch_event(&self, envelope: &hya_proto::Envelope) {
        self.events.lock().unwrap().push(envelope.clone());
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        self.leak.fetch_add(1, Ordering::SeqCst);
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        self.leak.fetch_add(1, Ordering::SeqCst);
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        self.leak.fetch_add(1, Ordering::SeqCst);
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        self.leak.fetch_add(1, Ordering::SeqCst);
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        self.tool_before.fetch_add(1, Ordering::SeqCst);
        let mut input = input.input;
        input["hooked"] = json!(true);
        ToolExecuteBeforeOutcome::Continue { input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        self.tool_after.fetch_add(1, Ordering::SeqCst);
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

struct HookedSidecarFactory {
    bindings: Arc<[ResolvedTool]>,
    hooks: Arc<dyn HookDispatcher>,
}

struct HookedSidecarHandle {
    bindings: Arc<[ResolvedTool]>,
    hooks: Arc<dyn HookDispatcher>,
}

#[async_trait]
impl BoundSidecarFactory for HookedSidecarFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(HookedSidecarHandle {
            bindings: self.bindings.clone(),
            hooks: self.hooks.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for HookedSidecarHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.bindings.clone()
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        Some(self.hooks.clone())
    }
}

#[derive(Clone, Copy)]
enum HookLossStage {
    Before,
    After,
    Event,
}

struct HookLossDispatcher {
    stage: HookLossStage,
    healthy: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl HookDispatcher for HookLossDispatcher {
    fn dispatch_event(&self, _envelope: &hya_proto::Envelope) {
        if matches!(self.stage, HookLossStage::Event)
            && self
                .healthy
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
        match self.stage {
            HookLossStage::Before => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.healthy.store(false, Ordering::SeqCst);
                ToolExecuteBeforeOutcome::Veto {
                    reason: "hook transport lost".to_string(),
                }
            }
            HookLossStage::After => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                ToolExecuteBeforeOutcome::Continue { input: input.input }
            }
            HookLossStage::Event => ToolExecuteBeforeOutcome::Continue { input: input.input },
        }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        if matches!(self.stage, HookLossStage::After) {
            self.healthy.store(false, Ordering::SeqCst);
        }
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

struct HookLossFactory {
    bindings: Arc<[ResolvedTool]>,
    hooks: Arc<HookLossDispatcher>,
    healthy: Arc<AtomicBool>,
    terminates: Arc<AtomicUsize>,
}

struct HookLossHandle {
    bindings: Arc<[ResolvedTool]>,
    hooks: Arc<HookLossDispatcher>,
    healthy: Arc<AtomicBool>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for HookLossFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(HookLossHandle {
            bindings: self.bindings.clone(),
            hooks: self.hooks.clone(),
            healthy: self.healthy.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for HookLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.bindings.clone()
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        Some(self.hooks.clone())
    }
}

struct ShutdownProbeFactory {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    member: MemberId,
    calls: Arc<AtomicUsize>,
    member_finished_at_shutdown: Arc<AtomicBool>,
}

struct ShutdownProbeHandle {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    member: MemberId,
    calls: Arc<AtomicUsize>,
    member_finished_at_shutdown: Arc<AtomicBool>,
}

#[async_trait]
impl BoundSidecarFactory for ShutdownProbeFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(ShutdownProbeHandle {
            engine: self.engine.clone(),
            lead: self.lead,
            member: self.member,
            calls: self.calls.clone(),
            member_finished_at_shutdown: self.member_finished_at_shutdown.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ShutdownProbeHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = self.engine.replay(self.lead).await?;
        let member_finished = events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::MemberFinished { member, .. } if member == &self.member
            )
        });
        self.member_finished_at_shutdown
            .store(member_finished, Ordering::SeqCst);
        Ok(())
    }
}

struct HookProbeTool {
    name: String,
    inputs: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for HookProbeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "activation hook probe".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        ctx.permission
            .assert(Action::Tool, Resource::Tool(self.name.clone()))
            .await?;
        self.inputs.lock().unwrap().push(input);
        Ok(json!({ "ok": true }))
    }
}

async fn assert_sidecar_failed(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    root: SessionId,
    child: SessionId,
    handle: &str,
) {
    let events = engine.replay(root).await.unwrap();
    assert_eq!(events.iter().filter(|e| matches!(&e.event,
        Event::SubagentReported { child: reported, outcome: hya_proto::ReportOutcome::Failed, report, .. }
        if *reported == child && report.contains("activation sidecar lost")
    )).count(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.event,
                Event::MailSent { from, body, .. }
                if from == handle && body.contains("activation sidecar lost")
            ))
            .count(),
        1
    );
    assert!(!events.iter().any(|e| matches!(&e.event, Event::AgentRestarted { child: restarted, .. } if *restarted == child)));
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection.team.archived[handle].reason,
        hya_proto::ArchiveReason::Reported
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(supervisor.episode_count(), 0);
}

async fn assert_stopped_member_revives(
    engine: &SessionEngine,
    root: SessionId,
    child: SessionId,
    handle: &str,
    body: &str,
) {
    engine
        .mail_send(
            root,
            MailEndpoint::parse(handle),
            MailKind::Message,
            body.into(),
        )
        .await
        .expect("engine mail must revive the stopped member");
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(&e.event, Event::AgentRestarted { child: restarted, epoch, .. } if *restarted == child && *epoch > hya_proto::ActorEpoch::INITIAL)));
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(projection.team.roster[handle].session, child);
    assert!(!projection.team.archived.contains_key(handle));
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(engine.read_projection(child).await.unwrap().session.messages.iter().any(|message| {
        message.role == Role::User && message.parts.iter().any(|part| matches!(part, PartProjection::Text { text, .. } if text.contains(body)))
    }));
}

async fn engine() -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

/// Records every provider request together with the session that issued it, so a
/// test can tell the lead's model input apart from a child's.
struct PerSessionRecorder {
    requests: Arc<Mutex<Vec<(SessionId, CompletionRequest)>>>,
}

#[async_trait]
impl Provider for PerSessionRecorder {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
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
        self.requests.lock().unwrap().push((session, request));
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("done".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// AC9: everything this task records is view-only. A child compacting, and the
/// directive recorded on the spawn edge, must not add a single byte to the
/// lead's model input.
#[tokio::test]
async fn recorded_observability_never_enters_the_parent_model_input() {
    let workdir = support::TestDir::new("observability-no-leak");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(PerSessionRecorder {
        requests: requests.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        // Tuned so ONLY the child crosses the threshold: its directive below is
        // deliberately long, while the lead's prompt stays short. If the lead
        // compacted too, its own marker would mask a real leak from the child.
        .with_compaction(
            Arc::new(LeakProbeSummarizer),
            hya_core::CompactionConfig {
                // The fake route advertises a 200k window, so the threshold is
                // window-scaled to 20k, above the lead's system and tool schemas.
                // The child's directive clears that; the lead's request does not.
                token_threshold: 1_000_000,
                keep_recent: 0,
                context_fraction: 0.1,
                ..hya_core::CompactionConfig::default()
            },
        ),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };
    let lead = engine
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

    // The child crosses 20k tokens even after counting the fixed request material.
    let directive = format!(
        "SECRET_DIRECTIVE_MARKER audit the retry paths. {}",
        "x".repeat(128_000)
    );
    let mut spec = member(&engine, &agent, &directive);
    spec.tool_call = Some(ToolCallId::new());
    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);

    // The child really did compact, otherwise this test proves nothing.
    let child = engine.read_projection(lead).await.unwrap().session.members[0]
        .child
        .unwrap();
    let child_events = engine.store().replay(child).await.unwrap();
    assert!(
        child_events
            .iter()
            .any(|e| matches!(e.event, Event::ContextCompacted { .. })),
        "precondition: the child must have compacted"
    );

    // Run a lead turn AFTER the child finished, so the lead's model input is
    // built from a log that already contains the spawn edge and the child's work.
    engine
        .admit_user_prompt(lead, "what did the member find?".to_string())
        .await
        .unwrap();
    engine
        .run_turn(lead, &agent, CancellationToken::new())
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    let lead_requests: Vec<_> = requests
        .iter()
        .filter(|(session, _)| *session == lead)
        .collect();
    assert!(!lead_requests.is_empty(), "the lead must have run a turn");
    for (_, request) in lead_requests {
        let rendered = format!("{:?}", request.messages);
        assert!(
            !rendered.contains("HYA_COMPACTED_CONTEXT"),
            "a child's compaction marker leaked into the lead: {rendered}"
        );
        assert!(
            !rendered.contains("LEAK_PROBE_SUMMARY"),
            "a child's summary leaked into the lead: {rendered}"
        );
        assert!(
            !rendered.contains("SECRET_DIRECTIVE_MARKER"),
            "the recorded directive leaked into the lead's messages: {rendered}"
        );
    }
}

struct LeakProbeSummarizer;

#[async_trait]
impl hya_core::Summarizer for LeakProbeSummarizer {
    async fn summarize(
        &self,
        _messages: &[hya_proto::Message],
        _options: hya_core::SummarizeOptions,
    ) -> Result<String, hya_core::CoreError> {
        Ok("LEAK_PROBE_SUMMARY".to_string())
    }
}

/// AC5 + AC6: the spawn edge must carry the member's purpose verbatim and the
/// tool call that produced it, for a fresh spawn and for a resumed session.
#[tokio::test]
async fn member_spawn_records_directive_verbatim_and_originating_tool_call() {
    let workdir = support::TestDir::new("spawn-records-directive");
    let (engine, mut agent) = engine().await;
    agent.workdir = workdir.path().to_path_buf();
    let lead = engine
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

    // A long, structured directive: the purpose must survive intact, not be
    // truncated to the short UI description the way `summary` is.
    let directive = "Audit every provider route for retry handling. \
                     Report each file, the retry policy it uses, and any route \
                     that silently swallows a transport error."
        .to_string();
    let call = ToolCallId::new();
    let mut spec = member(&engine, &agent, &directive);
    spec.description = "audit retries".to_string();
    spec.tool_call = Some(call);

    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);

    let projection = engine.read_projection(lead).await.unwrap();
    let member = &projection.session.members[0];
    assert_eq!(
        member.directive, directive,
        "the directive must be recorded verbatim, not truncated"
    );
    assert_eq!(
        member.tool_call,
        Some(call),
        "the spawn edge must anchor to its originating tool call"
    );
    // The short UI label stays separate from the full purpose.
    assert_eq!(member.description, "audit retries");

    // A resume re-emits MemberSpawned for the same member; the purpose recorded
    // by the original spawn must not be blanked by a thinner re-emit.
    let child = member.child.unwrap();
    let mut resumed = member_spec_for_resume(&engine, &agent, child);
    resumed.id = member.member;
    let _ = run_team(
        engine.clone(),
        lead,
        vec![resumed],
        CancellationToken::new(),
    )
    .await;
    let after = engine.read_projection(lead).await.unwrap();
    let member = after
        .session
        .members
        .iter()
        .find(|m| m.child == Some(child))
        .expect("resumed member row");
    assert_eq!(
        member.directive, directive,
        "a resume must not erase the original purpose"
    );
}

/// A resume spec: same child session, empty directive (the child already has it).
fn member_spec_for_resume(
    engine: &SessionEngine,
    agent: &AgentSpec,
    child: SessionId,
) -> MemberSpec {
    let mut spec = member(engine, agent, "");
    spec.session = Some(child);
    spec
}

#[tokio::test]
async fn pre_admitted_member_nested_spawn_completes() {
    let workdir = support::TestDir::new("pre-admitted-nested-spawn");
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "task".to_string(),
                input: json!({
                    "description": "nested task",
                    "prompt": "nested prompt",
                    "subagent_type": "hya-task"
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("member complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Task,
        "*",
        Mode::Allow,
    )]));
    let (spawn_sender, mut spawn_rx) = BoundSpawnSender::with_capacity(1);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            support::test_runtime(tools),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender),
    );
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "member harness".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };
    let agent = engine
        .agent_spec_for_binding(&binding, &base, "hya-task")
        .unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-task")
        .unwrap();
    let resources = binding.agent_resource_policy("hya-task").unwrap();
    let member = MemberSpec {
        id: MemberId::new(),
        agent,
        binding,
        agents,
        resources: Some(resources),
        guidance: None,
        directive: "member directive".to_string(),
        description: "member task".to_string(),
        session: None,
        sidecar_factory: None,
        tool_call: None,
    };
    let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
    let admission = AdmissionMemberIdentity {
        operation_id: operation.operation_id(),
        member_ordinal: 0,
    };
    let run = tokio::spawn(run_pre_admitted_member(
        engine,
        root,
        member,
        CancellationToken::new(),
        admission,
    ));

    let bound = tokio::time::timeout(std::time::Duration::from_secs(5), spawn_rx.recv())
        .await
        .expect("nested spawn request must arrive")
        .expect("spawn sender must remain connected");
    let (_binding, nested_request) = bound.into_parts();
    let nested_parent = nested_request.parent;
    nested_request
        .reply
        .send(Ok(vec![MemberOutcome {
            member: "nested-member".to_string(),
            session: SessionId::new().to_string(),
            status: "done".to_string(),
            summary: "nested complete".to_string(),
            model: None,
        }]))
        .expect("nested spawn reply must be accepted");

    let evidence = tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("pre-admitted member must complete")
        .expect("pre-admitted member task must not panic");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    let admitted_child = evidence[0]
        .session
        .parse::<SessionId>()
        .expect("member evidence must expose the admitted child session");
    assert_eq!(nested_parent, admitted_child);
}

/// A provider that records how many streams run concurrently, so a test can prove
/// the streaming-concurrency semaphore actually caps parallelism.
struct ConcurrencyProbeProvider {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for ConcurrencyProbeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("MEMBERTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

async fn governed_engine(
    limits: SubagentLimits,
    provider: Arc<dyn Provider>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(limits)),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

#[tokio::test]
async fn provider_streams_partition_100_general_28_reserved_and_root_progresses() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(TransientLossProvider {
        entered: entered.clone(),
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
    let governor = engine.governor().expect("governor installed").clone();

    assert_eq!(governor.limits().max_concurrency, 100);
    assert_eq!(governor.available_general_stream_permits(), 100);
    assert_eq!(governor.available_reserved_stream_permits(), 28);

    let mut general = Vec::with_capacity(100);
    for _ in 0..100 {
        general.push(
            governor
                .acquire_general_stream()
                .await
                .expect("general stream permit"),
        );
    }
    assert_eq!(governor.available_general_stream_permits(), 0);
    assert_eq!(governor.available_reserved_stream_permits(), 28);

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
    engine
        .admit_user_prompt(root, "root progress".to_string())
        .await
        .unwrap();
    let run = tokio::spawn({
        let engine = engine.clone();
        let agent = agent.clone();
        async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("root provider stream must progress under general saturation");
    assert_eq!(governor.available_general_stream_permits(), 0);
    assert_eq!(governor.available_reserved_stream_permits(), 27);
    assert!(
        engine
            .store()
            .nonterminal_admissions_for_root(root)
            .await
            .unwrap()
            .is_empty()
    );

    release.notify_one();
    assert_eq!(run.await.unwrap().unwrap(), FinishReason::Stop);
    assert_eq!(governor.available_reserved_stream_permits(), 28);

    let mut reserved = Vec::with_capacity(28);
    for _ in 0..28 {
        reserved.push(
            governor
                .acquire_reserved_stream()
                .await
                .expect("reserved stream permit"),
        );
    }
    assert_eq!(governor.available_general_stream_permits(), 0);
    assert_eq!(governor.available_reserved_stream_permits(), 0);

    drop(general.pop().expect("general permit to release"));
    assert_eq!(governor.available_general_stream_permits(), 1);
    assert_eq!(governor.available_reserved_stream_permits(), 0);
    general.push(
        governor
            .acquire_general_stream()
            .await
            .expect("general stream permit reacquisition"),
    );
    drop(reserved.pop().expect("reserved permit to release"));
    assert_eq!(governor.available_general_stream_permits(), 0);
    assert_eq!(governor.available_reserved_stream_permits(), 1);
    reserved.push(
        governor
            .acquire_reserved_stream()
            .await
            .expect("reserved stream permit reacquisition"),
    );
    drop(general);
    drop(reserved);
    assert_eq!(governor.available_general_stream_permits(), 100);
    assert_eq!(governor.available_reserved_stream_permits(), 28);

    assert!(
        engine
            .store()
            .nonterminal_admissions_for_root(root)
            .await
            .unwrap()
            .is_empty()
    );
}

fn member(engine: &SessionEngine, agent: &AgentSpec, directive: &str) -> MemberSpec {
    MemberSpec {
        id: MemberId::new(),
        agent: agent.clone(),
        binding: engine.bind_runtime(&agent.workdir).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: directive.to_string(),
        description: String::new(),
        session: None,
        sidecar_factory: None,
        tool_call: None,
    }
}

#[tokio::test]
async fn sidecar_ack_precedes_running_state_provider_poll_and_task_admission() {
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = Arc::new(Notify::new());
    let provider = Arc::new(AckProbeProvider {
        polls: polls.clone(),
        polled,
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let (start_tx, mut start_rx) = mpsc::unbounded_channel();
    let ready = Arc::new(Notify::new());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(AckGateFactory {
        starts: start_tx,
        ready: ready.clone(),
    });
    let mut spec = member(&engine, &agent, "harness-owned task");
    spec.sidecar_factory = Some(factory);

    let run = tokio::spawn(run_team(
        engine.clone(),
        lead,
        vec![spec],
        CancellationToken::new(),
    ));
    let start = start_rx.recv().await.unwrap();
    let SidecarStart {
        activation_id,
        lifecycle,
    } = start;
    assert!(!activation_id.is_empty());
    assert!(matches!(lifecycle, SidecarLifecycle::Transient));
    assert_eq!(polls.load(Ordering::SeqCst), 0);

    let lead_projection = engine.read_projection(lead).await.unwrap();
    let member_projection = lead_projection.session.members.first().unwrap();
    assert_eq!(member_projection.status, MemberRunStatus::Spawning);
    assert!(!matches!(
        member_projection.status,
        MemberRunStatus::Running
    ));
    let child = member_projection.child.unwrap();
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(
        !child_projection
            .session
            .messages
            .iter()
            .any(|message| matches!(message.role, Role::User))
    );

    ready.notify_one();
    let evidence = run.await.unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    assert_eq!(polls.load(Ordering::SeqCst), 1);

    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(child_projection.session.messages.iter().any(|message| {
        matches!(message.role, Role::User)
            && message.parts.iter().any(|part| {
                matches!(
                    part,
                    PartProjection::Text { text, .. } if text == "harness-owned task"
                )
            })
    }));
    assert!(child_projection.session.messages.iter().any(|message| {
        matches!(message.role, Role::Assistant)
            && message.parts.iter().any(|part| {
                matches!(
                    part,
                    PartProjection::Text { text, .. } if text == "ACKTEXT"
                )
            })
    }));
}

#[tokio::test]
async fn bundle_sidecar_tool_permission_denial_prevents_dispatch() {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(agent_catalog(sidecar_permission_bundle()).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(SidecarPermissionTool {
            name: canonical.to_string(),
            calls: calls.clone(),
        }),
        permission: ToolPermission::Tool,
    };
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![sidecar_tool]);
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ImmediateSidecarFactory { bindings });

    let provider = Arc::new(SidecarPermissionProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Deny,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        runtime,
        permission,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("sidecar-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "sidecar permission".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let lead = engine
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = Some(binding.agent_resource_policy("sidecar-agent").unwrap());
    let spec = MemberSpec {
        id: MemberId::new(),
        agent,
        binding,
        agents: Arc::from([]),
        resources,
        guidance: None,
        directive: "exercise denied sidecar tool".to_string(),
        description: "sidecar permission".to_string(),
        session: None,
        sidecar_factory: Some(factory),
        tool_call: None,
    };

    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    {
        let requests = provider.requests.lock().unwrap();
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|schema| schema.name.as_str() == "echo")
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let lead_projection = engine.read_projection(lead).await.unwrap();
    let child = lead_projection.session.members[0].child.unwrap();
    let events = engine.store().replay(child).await.unwrap();
    assert!(events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, .. } if message_text.contains("permission denied")
        )
    }));
}

#[tokio::test]
async fn activation_bound_sidecar_hooks_mutate_tool_and_observe_only_child_events() {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(agent_catalog(sidecar_permission_bundle()).unwrap());
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(HookProbeTool {
            name: canonical.to_string(),
            inputs: inputs.clone(),
        }),
        permission: ToolPermission::Tool,
    };
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![sidecar_tool]);
    let hooks = Arc::new(ActivationHookProbe::default());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(HookedSidecarFactory {
        bindings,
        hooks: hooks.clone(),
    });

    let provider = Arc::new(SidecarPermissionProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        runtime,
        permission,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("sidecar-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "sidecar hooks".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let lead = engine
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = Some(binding.agent_resource_policy("sidecar-agent").unwrap());
    let spec = MemberSpec {
        id: MemberId::new(),
        agent,
        binding,
        agents: Arc::from([]),
        resources,
        guidance: None,
        directive: "exercise activation hooks".to_string(),
        description: "activation hooks".to_string(),
        session: None,
        sidecar_factory: Some(factory),
        tool_call: None,
    };

    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);

    let lead_projection = engine.read_projection(lead).await.unwrap();
    let child = lead_projection.session.members[0].child.unwrap();
    let events = engine.store().replay(child).await.unwrap();
    let tool_results = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results, vec![json!({ "ok": true })]);

    assert_eq!(
        inputs.lock().unwrap().as_slice(),
        &[json!({ "hooked": true })]
    );
    assert_eq!(hooks.tool_before.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.tool_after.load(Ordering::SeqCst), 1);
    let captured_events = hooks.events.lock().unwrap();
    assert!(!captured_events.is_empty());
    assert!(
        captured_events
            .iter()
            .all(|envelope| envelope.event.session() == Some(child))
    );
    assert_eq!(hooks.leak.load(Ordering::SeqCst), 0);
}

/// Counts the hooks an installed Plugin bundle's dispatcher receives.
#[derive(Clone, Default)]
struct PluginHookProbe {
    chat_params: Arc<AtomicUsize>,
    tool_before: Arc<AtomicUsize>,
}

#[async_trait]
impl HookDispatcher for PluginHookProbe {
    fn dispatch_event(&self, _envelope: &hya_proto::Envelope) {}

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
        self.chat_params.fetch_add(1, Ordering::SeqCst);
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        self.tool_before.fetch_add(1, Ordering::SeqCst);
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

/// An installed Plugin's hooks reach a bundle subagent, and they compose with
/// (never replace) the activation sidecar's restricted hooks scoped around the
/// member run.
#[tokio::test]
async fn plugin_hooks_reach_bundle_subagent_alongside_its_sidecar_hooks() {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let plugin = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "plugin",
        vec![hya_bundle::SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nidentity: { id: acme/router, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let bundles = [sidecar_permission_bundle(), plugin.bundles()[0].clone()];
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&bundles).unwrap())).unwrap(),
    );
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(HookProbeTool {
            name: canonical.to_string(),
            inputs: inputs.clone(),
        }),
        permission: ToolPermission::Tool,
    };
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![sidecar_tool]);
    let sidecar = Arc::new(ActivationHookProbe::default());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(HookedSidecarFactory {
        bindings,
        hooks: sidecar.clone(),
    });
    let provider = Arc::new(SidecarPermissionProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let plugin_hooks = PluginHookProbe::default();
    runtime
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                hya_core::RuntimeSource::new(
                    hya_core::RuntimeSourceId::bundle("acme/router"),
                    [9; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(Arc::new(plugin_hooks.clone())),
            ])
        })
        .unwrap();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        runtime,
        permission,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("sidecar-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "sidecar hooks".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = Some(binding.agent_resource_policy("sidecar-agent").unwrap());
    let spec = MemberSpec {
        id: MemberId::new(),
        agent,
        binding,
        agents: Arc::from([]),
        resources,
        guidance: None,
        directive: "exercise plugin and sidecar hooks".to_string(),
        description: "plugin hooks".to_string(),
        session: None,
        sidecar_factory: Some(factory),
        tool_call: None,
    };

    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    assert_eq!(
        plugin_hooks.chat_params.load(Ordering::SeqCst),
        2,
        "the Plugin's chat.params runs once per bundle-agent provider round"
    );
    assert_eq!(plugin_hooks.tool_before.load(Ordering::SeqCst), 1);
    assert_eq!(
        inputs.lock().unwrap().as_slice(),
        &[json!({ "hooked": true })],
        "the sidecar's tool.execute.before still rewrites the input"
    );
    assert_eq!(sidecar.tool_before.load(Ordering::SeqCst), 1);
    assert_eq!(sidecar.tool_after.load(Ordering::SeqCst), 1);
    assert_eq!(
        sidecar.leak.load(Ordering::SeqCst),
        0,
        "the sidecar stays clamped to its three hooks"
    );
}

#[tokio::test]
async fn resident_sidecar_tool_binding_reaches_captured_turn_view() {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(agent_catalog(sidecar_permission_bundle()).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(SidecarPermissionTool {
            name: canonical.to_string(),
            calls: calls.clone(),
        }),
        permission: ToolPermission::Tool,
    };
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![sidecar_tool]);
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ImmediateSidecarFactory { bindings });

    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(SidecarPermissionProvider {
        requests: requests.clone(),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let bus = EventBus::default();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        runtime,
        permission,
        bus.clone(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("sidecar-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "sidecar permission".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("sidecar-agent").unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = bus.subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident task".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Failed | RosterStatus::Idle,
                ..
            } if *session == root && event_handle == &handle
        ) {
            break;
        }
    }
    supervisor.team_cancel(root).unwrap().cancel();

    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection.team.roster.get(handle.as_str()).unwrap().status,
        RosterStatus::Idle
    );
    {
        let requests = requests.lock().unwrap();
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|schema| schema.name.as_str() == "echo")
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let events = engine.store().replay(child).await.unwrap();
    assert!(events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output == &json!({"ok": true})
        )
    }));
}

#[tokio::test]
async fn explicit_idle_resident_stop_is_final_idempotent_and_releases_claim() {
    let (engine, agent) = engine().await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy: healthy.clone(),
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        terminated: None,
        loss: None,
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident stop probe".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root && event_handle == &handle
        ) {
            break;
        }
    }
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert!(healthy.load(Ordering::SeqCst));

    supervisor.end_member(root, &handle).await.unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );

    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(handle.as_str()));
    assert_eq!(projection.team.archived[handle.as_str()].session, child);
    assert_eq!(supervisor.episode_count(), 0);

    assert_stopped_member_revives(&engine, root, child, &handle, "after stop").await;
}

#[tokio::test]
async fn resident_registration_failure_releases_claim_and_leaves_no_slot() {
    let db_dir = support::TestDir::new("resident-registration-failure");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect(&db_path).await.unwrap(),
        router,
        support::test_runtime(tools),
        permission,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
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
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE test_resident_registration_failure_gate (id INTEGER PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO test_resident_registration_failure_gate (id) VALUES (1)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_resident_registration_failure \
         BEFORE INSERT ON event_log \
         WHEN EXISTS (SELECT 1 FROM test_resident_registration_failure_gate) \
         BEGIN SELECT RAISE(ABORT, 'test registration append failure'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let result = supervisor
        .register_existing_resident(
            root,
            child,
            "registration-failure-1".to_string(),
            agent,
            None,
        )
        .await;
    assert!(result.is_err());
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_none());
    assert_eq!(
        engine
            .store()
            .replay(root)
            .await
            .unwrap()
            .into_iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::AgentRegistered {
                        handle,
                        agent_session,
                        ..
                    } if handle == "registration-failure-1" && *agent_session == child
                )
            })
            .count(),
        0
    );
}

#[tokio::test]
async fn explicit_stop_fails_closed_instead_of_taking_over_a_newer_resident_claim() {
    let (engine, agent) = engine().await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let newer = engine
        .store()
        .recover_claim(child, hya_proto::OwnerRunId::new())
        .await
        .unwrap();
    let result = supervisor.end_member(root, &handle).await;
    assert!(matches!(
        result,
        Err(CoreError::Store(hya_store::StoreError::StaleActorClaim { actor_id }))
            if actor_id == child
    ));
    assert!(
        engine
            .store()
            .validate_actor_claim(&newer.claim)
            .await
            .is_ok()
    );
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );

    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(handle.as_str()).unwrap();
    assert_eq!(entry.status, RosterStatus::Idle);
    assert_ne!(entry.current_task.as_deref(), Some("resident stopped"));
}

#[tokio::test]
async fn resident_direct_send_committed_before_stop_remains_unread() {
    let db_dir = support::TestDir::new("resident-direct-stop");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let make_engine = |store: SessionStore, bus: EventBus| {
        let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
        let tools = Arc::new(ToolRegistry::builtins());
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        Arc::new(SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            permission,
            bus,
        ))
    };

    let primary = make_engine(
        SessionStore::connect(&db_path).await.unwrap(),
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let root = primary
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
    let binding = primary.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let supervisor = ResidentSupervisor::start(primary.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let before_send = primary.read_projection(root).await.unwrap();
    assert_eq!(
        before_send.team.roster.get(handle.as_str()).unwrap().status,
        RosterStatus::Idle
    );

    let sender = make_engine(
        SessionStore::connect(&db_path).await.unwrap(),
        EventBus::default(),
    );
    sender
        .admit_user_prompt(root, "admit sender binding".to_string())
        .await
        .unwrap();
    let release = Arc::new(Notify::new());
    let ready = Arc::new(Barrier::new(2));
    let (committed_tx, mut committed_rx) = mpsc::unbounded_channel();
    let sender_task = tokio::spawn({
        let release = release.clone();
        let ready = ready.clone();
        let sender = sender.clone();
        let handle = handle.clone();
        async move {
            ready.wait().await;
            release.notified().await;
            let result = sender
                .mail_send(
                    root,
                    MailEndpoint::Handle(handle),
                    MailKind::Message,
                    "mail accepted before stop".to_string(),
                )
                .await;
            let _ = committed_tx.send(result);
        }
    });
    ready.wait().await;
    release.notify_one();
    let receipt = committed_rx
        .recv()
        .await
        .expect("sender must signal its committed mail")
        .expect("direct mail must commit");
    assert_eq!(receipt.recipients, 1);

    let stop_result = supervisor.end_member(root, &handle).await;
    sender_task.await.unwrap();
    assert!(
        stop_result.is_ok(),
        "resident stop must succeed: {stop_result:?}"
    );
    assert!(
        !primary
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );

    let events = primary.store().replay(root).await.unwrap();
    let mail_events = events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::MailSent { session, .. } if *session == root
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(mail_events.len(), 1);
    assert!(matches!(
        &mail_events[0].event,
        Event::MailSent {
            from,
            to: MailEndpoint::Handle(recipient),
            kind: MailKind::Message,
            body,
            ..
        } if from == "main" && recipient == &handle && body == "mail accepted before stop"
    ));

    let projection = primary.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(handle.as_str()));
    assert_eq!(projection.team.archived[handle.as_str()].session, child);
    let inbox_len = projection.team.inboxes.get(&handle).map_or(0, Vec::len);
    assert_eq!(inbox_len, 1);
    let mut before_archive = hya_proto::Projection::default();
    for envelope in primary.replay(root).await.unwrap() {
        if matches!(&envelope.event, Event::AgentArchived { handle: archived, .. } if archived == &handle)
        {
            break;
        }
        before_archive.apply(&envelope);
    }
    assert_eq!(
        before_archive.team.roster[&handle].resident_cursor, 0,
        "stopping must not mark unread mail consumed"
    );
}

#[tokio::test]
async fn message_budget_kill_terminates_sidecar_and_removes_resident_slot() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            per_team_message_budget: 1,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy,
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        terminated: Some(terminated.clone()),
        loss: None,
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "first message".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root && event_handle == &handle
        ) {
            break;
        }
    }
    assert_eq!(ready.load(Ordering::SeqCst), 1);

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "budget trip".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Failed,
                current_task: Some(task),
            } if *session == root
                && event_handle == &handle
                && task.contains("message budget exceeded")
        ) {
            break;
        }
    }

    tokio::time::timeout(std::time::Duration::from_millis(250), terminated.notified())
        .await
        .expect("resident sidecar termination must complete after team kill");
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(handle.as_str()));
    assert_eq!(projection.team.archived[handle.as_str()].session, child);
    assert!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .any(|envelope| matches!(
                &envelope.event,
                Event::AgentActivityChanged { handle: event_handle, current_task: Some(task), .. }
                if event_handle == &handle && task.contains("message budget exceeded")
            ))
    );
    assert!(supervisor.team_cancel(root).is_none());
}

#[tokio::test]
async fn explicit_running_resident_stop_is_idempotent_fences_and_drops_queued_mail() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls: polls.clone(),
        entered: entered_tx,
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready,
        healthy,
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        terminated: None,
        loss: None,
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident running stop".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "queued after stop".to_string(),
        )
        .await
        .unwrap();

    let first_supervisor = supervisor.clone();
    let first_handle = handle.clone();
    let first_stop =
        tokio::spawn(async move { first_supervisor.end_member(root, &first_handle).await });
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(envelope.event, Event::MessageFinished { session, cause: Some(hya_proto::FinishCause::Archived), .. } if session == child)
        {
            break;
        }
    }
    let second_stop = supervisor.end_member(root, &handle);
    release.notify_one();
    let first_result = first_stop.await.unwrap();
    let second_result = second_stop.await;

    assert!(
        second_result.is_ok(),
        "second stop must be idempotent while first cleanup is pending: {second_result:?}"
    );
    assert!(
        first_result.is_ok(),
        "first running resident stop must complete: {first_result:?}"
    );
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(supervisor.episode_count(), 0);

    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(handle.as_str()));
    assert_eq!(projection.team.archived[handle.as_str()].session, child);
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(!child_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            matches!(
                part,
                PartProjection::Text { text, .. } if text.contains("queued after stop")
            )
        })
    }));
    assert!(!child_projection.session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, PartProjection::Text { text, .. } if text.contains("RESIDENT_STOP_TEXT")))));

    assert_stopped_member_revives(&engine, root, child, &handle, "after stop").await;
}

#[tokio::test]
async fn resident_replacement_declaration_drift_disables_once_and_releases_claim() {
    let polls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(AckProbeProvider {
        polls: polls.clone(),
        polled: Arc::new(Notify::new()),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(true));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(DeclarationDriftFactory {
        starts: starts.clone(),
        healthy: healthy.clone(),
        terminates: terminates.clone(),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident declaration drift first".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root && event_handle == &handle
        ) {
            break;
        }
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    healthy.store(false, Ordering::SeqCst);

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident declaration drift second".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if let Event::AgentActivityChanged {
            session,
            handle: event_handle,
            status: RosterStatus::Failed,
            current_task: Some(task),
        } = &envelope.event
            && *session == root
            && event_handle == &handle
            && task.contains("bundle sidecar declaration drift")
        {
            break;
        }
    }
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(supervisor.episode_count(), 0);

    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(!child_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            matches!(
                part,
                PartProjection::Text { text, .. }
                    if text.contains("declaration drift second")
            )
        })
    }));
    assert_stopped_member_revives(
        &engine,
        root,
        child,
        &handle,
        "resident declaration drift third",
    )
    .await;
}

#[tokio::test]
async fn resident_activation_hook_transport_loss_reports_failure() {
    assert_resident_hook_transport_loss(HookLossStage::Before).await;
}

async fn assert_resident_hook_transport_loss(stage: HookLossStage) {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(agent_catalog(sidecar_permission_bundle()).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let sidecar_tool = ResolvedTool {
        tool: Arc::new(SidecarPermissionTool {
            name: canonical.to_string(),
            calls: calls.clone(),
        }),
        permission: ToolPermission::Tool,
    };
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![sidecar_tool]);
    let healthy = Arc::new(AtomicBool::new(true));
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(HookLossDispatcher {
        stage,
        healthy: healthy.clone(),
        calls: hook_calls.clone(),
    });
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(HookLossFactory {
        bindings,
        hooks,
        healthy,
        terminates: terminates.clone(),
    });

    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(SidecarPermissionProvider {
        requests: requests.clone(),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let bus = EventBus::default();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        runtime,
        permission,
        bus.clone(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("sidecar-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "sidecar hook loss".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("sidecar-agent").unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = bus.subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident hook loss".to_string(),
        )
        .await
        .unwrap();
    let (observed_status, observed_task) = loop {
        let envelope = events.recv().await.unwrap();
        if let Event::AgentActivityChanged {
            session,
            handle: event_handle,
            status,
            current_task,
        } = &envelope.event
            && *session == root
            && event_handle == &handle
            && matches!(status, RosterStatus::Failed | RosterStatus::Idle)
        {
            break (*status, current_task.clone());
        }
    };

    assert_eq!(observed_status, RosterStatus::Failed);
    assert_eq!(observed_task.as_deref(), Some("activation sidecar lost"));
    assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    let expected_calls = match stage {
        HookLossStage::Before => 0,
        HookLossStage::After => 1,
        HookLossStage::Event => 0,
    };
    assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
    let expected_requests = if matches!(stage, HookLossStage::Event) {
        0
    } else {
        1
    };
    assert_eq!(requests.lock().unwrap().len(), expected_requests);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);

    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    let child_events = engine.store().replay(child).await.unwrap();
    assert!(
        !child_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::ToolResult { .. }))
    );
    assert!(!child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, .. } if message_text.contains("blocked by plugin")
        )
    }));
    assert_stopped_member_revives(&engine, root, child, &handle, "after hook sidecar loss").await;
    supervisor.end_member(root, &handle).await.unwrap();
}

#[tokio::test]
async fn resident_after_hook_transport_loss_fences_result_before_commit() {
    assert_resident_hook_transport_loss(HookLossStage::After).await;
}

#[tokio::test]
async fn resident_event_transport_loss_stops_before_model_poll() {
    assert_resident_hook_transport_loss(HookLossStage::Event).await;
}

#[tokio::test]
async fn resident_mailbox_message_waits_for_sidecar_ack_before_running() {
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = Arc::new(Notify::new());
    let provider = Arc::new(AckProbeProvider {
        polls: polls.clone(),
        polled: polled.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());

    let (start_tx, mut start_rx) = mpsc::unbounded_channel();
    let ready = Arc::new(Notify::new());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(AckGateFactory {
        starts: start_tx,
        ready: ready.clone(),
    });
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, member_handle, _) = supervisor
        .spawn_resident_parked(
            root,
            agent.clone(),
            (binding, Arc::from([]), resources, Some(factory)),
            "sidecar test".into(),
            None,
            None,
        )
        .await
        .unwrap();

    assert!(matches!(
        start_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 0);

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(member_handle.clone()),
            MailKind::Message,
            "resident task".to_string(),
        )
        .await
        .unwrap();
    let start = start_rx.recv().await.unwrap();
    let SidecarStart {
        activation_id,
        lifecycle,
    } = start;
    assert!(!activation_id.is_empty());
    assert!(matches!(lifecycle, SidecarLifecycle::Resident));
    assert_eq!(polls.load(Ordering::SeqCst), 0);

    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(
        !child_projection
            .session
            .messages
            .iter()
            .any(|message| matches!(message.role, Role::User))
    );
    let root_projection = engine.read_projection(root).await.unwrap();
    let worker = root_projection.team.roster.get(&member_handle).unwrap();
    assert_eq!(worker.status, RosterStatus::Idle);
    assert!(!matches!(worker.status, RosterStatus::Busy));

    ready.notify_one();
    polled.notified().await;
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(child_projection.session.messages.iter().any(|message| {
        matches!(message.role, Role::User)
            && message.parts.iter().any(|part| {
                matches!(
                    part,
                    PartProjection::Text { text, .. } if text == "[mail from main] resident task"
                )
            })
    }));
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn transient_sidecar_loss_interrupts_running_member_before_provider_release() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(TransientLossProvider {
        entered: entered.clone(),
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let healthy = Arc::new(AtomicBool::new(false));
    let loss = CancellationToken::new();
    let terminated = Arc::new(Notify::new());
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(TransientLossFactory {
        healthy: healthy.clone(),
        loss: loss.clone(),
        terminated: terminated.clone(),
        terminates: terminates.clone(),
    });
    let mut spec = member(&engine, &agent, "blocked provider turn");
    let member_id = spec.id;
    spec.sidecar_factory = Some(factory);

    let mut events = engine.bus().subscribe();
    let run = tokio::spawn(run_team(engine, lead, vec![spec], CancellationToken::new()));
    entered.notified().await;
    healthy.store(false, Ordering::SeqCst);
    loss.cancel();

    let observed = tokio::time::timeout(std::time::Duration::from_millis(250), async {
        let mut finished = false;
        let mut terminated_seen = false;
        while !(finished && terminated_seen) {
            tokio::select! {
                _ = terminated.notified(), if !terminated_seen => terminated_seen = true,
                envelope = events.recv(), if !finished => {
                    let envelope = envelope.unwrap();
                    if matches!(
                        &envelope.event,
                        Event::MemberFinished {
                            member,
                            status: MemberRunStatus::Failed | MemberRunStatus::Cancelled,
                            ..
                        } if *member == member_id
                    ) {
                        finished = true;
                    }
                }
            }
        }
    })
    .await;

    release.notify_one();
    let evidence = run.await.unwrap();
    assert!(
        observed.is_ok(),
        "sidecar loss must terminate the member before provider release"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
}

#[tokio::test]
async fn resident_sidecar_ready_failure_terminates_handle_once_and_removes_slot() {
    let (engine, agent) =
        governed_engine(SubagentLimits::default(), Arc::new(SelectiveFakeProvider)).await;
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
    let supervisor = ResidentSupervisor::start(engine.clone());
    let ready = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(AckFailureFactory {
        ready: ready.clone(),
        terminates: terminates.clone(),
        shutdowns: shutdowns.clone(),
    });
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, member_handle, _) = supervisor
        .spawn_resident_parked(
            root,
            agent.clone(),
            (binding, Arc::from([]), resources, Some(factory)),
            "sidecar test".into(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(member_handle.clone()),
            MailKind::Message,
            "resident task".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle,
                status: RosterStatus::Failed,
                current_task: Some(task),
            } if *session == root
                // Activity events carry the canonical path; only
                // `AgentRegistered.handle` is still the bare leaf.
                && handle == &member_handle
                && task.contains("sidecar ACK rejected")
        ) {
            break;
        }
    }

    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(supervisor.episode_count(), 0);
}

#[tokio::test]
async fn idle_resident_sidecar_loss_is_reaped_before_next_mail() {
    let (engine, agent) =
        governed_engine(SubagentLimits::default(), Arc::new(SelectiveFakeProvider)).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let loss = CancellationToken::new();
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy,
        shutdowns,
        terminates: terminates.clone(),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "first message".to_string(),
        )
        .await
        .unwrap();
    loop {
        let envelope = events.recv().await.unwrap();
        if matches!(
            &envelope.event,
            Event::AgentActivityChanged {
                session,
                handle: event_handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root && event_handle == &handle
        ) {
            break;
        }
    }
    assert_eq!(ready.load(Ordering::SeqCst), 1);

    loss.cancel();
    let observed =
        tokio::time::timeout(std::time::Duration::from_millis(250), terminated.notified()).await;
    let slot_remains = supervisor.team_cancel(root).is_some();
    let claim_remains = engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&child);
    if let Some(cancel) = supervisor.team_cancel(root) {
        cancel.cancel();
    }

    assert!(
        observed.is_ok(),
        "idle resident sidecar loss must reap before the next mail"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(slot_remains);
    assert!(claim_remains);
}

#[tokio::test]
async fn resident_prework_sidecar_loss_reports_failure_with_unread_mail() {
    assert_prework_loss_reports_failure(false).await;
}

#[tokio::test]
async fn resident_initial_directive_is_closed_after_prework_sidecar_loss() {
    assert_prework_loss_reports_failure(true).await;
}

async fn assert_prework_loss_reports_failure(initial: bool) {
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = Arc::new(Notify::new());
    let provider = Arc::new(AckProbeProvider {
        polls: polls.clone(),
        polled: polled.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let first_loss = CancellationToken::new();
    first_loss.cancel();
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentPreworkLossFactory {
        starts: starts.clone(),
        terminates: terminates.clone(),
        shutdowns: shutdowns.clone(),
        terminated: terminated.clone(),
        first_loss,
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let mut events = engine.bus().subscribe();
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            if initial {
                "initial directive survives".into()
            } else {
                String::new()
            },
            None,
            None,
        )
        .await
        .unwrap();

    if !initial {
        engine
            .mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "rearm after prework loss".to_string(),
            )
            .await
            .unwrap();
    }
    tokio::time::timeout(RESIDENT_END_BOUND, terminated.notified())
        .await
        .expect("first prework sidecar loss must terminate deterministically");

    tokio::time::timeout(RESIDENT_END_BOUND, async {
        loop {
            let envelope = events.recv().await.unwrap();
            if matches!(&envelope.event, Event::AgentArchived { child: archived, .. } if *archived == child) {
                break;
            }
        }
    }).await.expect("prework loss must end the episode");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    let projection = engine.read_projection(child).await.unwrap();
    if initial {
        assert!(
            projection
                .session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant && m.finish == Some(FinishReason::Cancelled))
        );
    } else {
        assert!(
            !projection
                .session
                .messages
                .iter()
                .any(|m| m.role == Role::User)
        );
    }
    assert_stopped_member_revives(
        &engine,
        root,
        child,
        &handle,
        "new episode after prework loss",
    )
    .await;
    supervisor.end_member(root, &handle).await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn running_resident_sidecar_loss_aborts_before_provider_release() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls: polls.clone(),
        entered: entered_tx,
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let loss = CancellationToken::new();
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy,
        shutdowns,
        terminates: terminates.clone(),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "running loss".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");
    loss.cancel();

    let observed = tokio::time::timeout(RESIDENT_END_BOUND, async {
        let mut terminated_seen = false;
        let mut failure_seen = false;
        while !(terminated_seen && failure_seen) {
            tokio::select! {
                _ = terminated.notified(), if !terminated_seen => terminated_seen = true,
                envelope = events.recv(), if !failure_seen => {
                    let envelope = envelope.unwrap();
                    if matches!(
                        &envelope.event,
                        Event::AgentActivityChanged {
                            session,
                            handle: event_handle,
                            status: RosterStatus::Failed,
                            current_task: Some(task),
                        } if *session == root
                            && event_handle == &handle
                            && task == "activation sidecar lost"
                    ) {
                        failure_seen = true;
                    }
                }
            }
        }
    })
    .await;
    let terminated_before_release = terminates.load(Ordering::SeqCst);
    let episodes_before_release = supervisor.episode_count();
    let claim_remains_before_release = engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&child);

    release.notify_one();
    let _ = supervisor.end_member(root, &handle).await;
    let child_events = engine.store().replay(child).await.unwrap();
    let successful_provider_output = child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::TextReplace { text, .. } if text.contains("RESIDENT_STOP_TEXT")
        )
    });

    assert!(
        observed.is_ok(),
        "running loss must end before provider release"
    );
    assert_eq!(terminated_before_release, 1);
    assert_eq!(episodes_before_release, 0);
    assert!(!claim_remains_before_release);
    assert!(!successful_provider_output);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    assert_stopped_member_revives(&engine, root, child, &handle, "after running sidecar loss")
        .await;
    supervisor.end_member(root, &handle).await.unwrap();
}

#[tokio::test]
async fn resident_running_loss_never_takes_over_its_claim() {
    let db_dir = support::TestDir::new("resident-running-loss-recover-claim-failure");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls: polls.clone(),
        entered: entered_tx,
        release: release.clone(),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect(&db_path).await.unwrap(),
            router,
            support::test_runtime(tools),
            permission,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let loss = CancellationToken::new();
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy: healthy.clone(),
        shutdowns,
        terminates: terminates.clone(),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "running loss recover claim failure".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert!(healthy.load(Ordering::SeqCst));

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_resident_recover_claim_failure \
         BEFORE UPDATE OF epoch, owner_run_id ON resident_actor_claim \
         WHEN OLD.state = 'active' AND NEW.state = 'active' \
              AND NEW.epoch = OLD.epoch + 1 \
         BEGIN SELECT RAISE(ABORT, 'test recover claim boundary'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    loss.cancel();

    let observed = tokio::time::timeout(RESIDENT_END_BOUND, async {
        let mut terminated_seen = false;
        let mut failure_seen = false;
        while !(terminated_seen && failure_seen) {
            tokio::select! {
                _ = terminated.notified(), if !terminated_seen => terminated_seen = true,
                envelope = events.recv(), if !failure_seen => {
                    let envelope = envelope.unwrap();
                    if matches!(
                        &envelope.event,
                        Event::AgentActivityChanged {
                            session,
                            handle: event_handle,
                            status: RosterStatus::Failed,
                            current_task: Some(task),
                        } if *session == root
                            && event_handle == &handle
                            && task.starts_with("activation sidecar lost")
                    ) {
                        failure_seen = true;
                    }
                }
            }
        }
    })
    .await;

    release.notify_one();
    let matching_failures = engine
        .store()
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == &handle
                    && task.starts_with("activation sidecar lost")
            )
        })
        .count();
    let claim_released = !engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&child);
    let child_events = engine.store().replay(child).await.unwrap();
    let successful_provider_output = child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::TextReplace { text, .. } if text.contains("RESIDENT_STOP_TEXT")
        )
    });

    assert!(
        observed.is_ok(),
        "sidecar loss must finalize its claim without takeover"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(claim_released);
    assert_eq!(supervisor.episode_count(), 0);
    assert_eq!(matching_failures, 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(!successful_provider_output);
    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    assert_stopped_member_revives(&engine, root, child, &handle, "after running sidecar loss")
        .await;
    supervisor.end_member(root, &handle).await.unwrap();
}

#[tokio::test]
async fn resident_running_loss_never_runs_recovery_transaction() {
    let db_dir = support::TestDir::new("resident-running-loss-recovery-transaction-failure");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls: polls.clone(),
        entered: entered_tx,
        release: release.clone(),
    });
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect(&db_path).await.unwrap(),
            router,
            support::test_runtime(tools),
            permission,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(Notify::new());
    let loss = CancellationToken::new();
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy: healthy.clone(),
        shutdowns,
        terminates: terminates.clone(),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "running loss recovery transaction failure".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert!(healthy.load(Ordering::SeqCst));

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    let trigger = format!(
        "CREATE TRIGGER test_resident_recovery_transaction_failure \
         BEFORE INSERT ON event_log \
         WHEN instr(NEW.payload, '\"type\":\"agent_activity_changed\"') > 0 \
           AND instr(NEW.payload, '\"session\":\"{root}\"') > 0 \
           AND instr(NEW.payload, '\"current_task\":\"aborted by resident recovery\"') > 0 \
         BEGIN SELECT RAISE(ABORT, 'test recovery transaction boundary'); END;"
    );
    sqlx::query(&trigger)
        .execute(&mut connection)
        .await
        .unwrap();
    loss.cancel();

    let observed = tokio::time::timeout(RESIDENT_END_BOUND, async {
        let mut terminated_seen = false;
        let mut failure_seen = false;
        while !(terminated_seen && failure_seen) {
            tokio::select! {
                _ = terminated.notified(), if !terminated_seen => terminated_seen = true,
                envelope = events.recv(), if !failure_seen => {
                    let envelope = envelope.unwrap();
                    if matches!(
                        &envelope.event,
                        Event::AgentActivityChanged {
                            session,
                            handle: event_handle,
                            status: RosterStatus::Failed,
                            current_task: Some(task),
                        } if *session == root
                            && event_handle == &handle
                            && task.starts_with("activation sidecar lost")
                    ) {
                        failure_seen = true;
                    }
                }
            }
        }
    })
    .await;

    let child_replay_at_failure = engine.store().replay(child).await.unwrap();
    release.notify_one();
    let root_events = engine.store().replay(root).await.unwrap();
    let matching_failures = root_events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == &handle
                    && task.starts_with("activation sidecar lost")
            )
        })
        .count();
    let aborted_recovery_events = root_events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == &handle
                    && task == "aborted by resident recovery"
            )
        })
        .count();
    let claim_released = !engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&child);
    let child_events = engine.store().replay(child).await.unwrap();
    let successful_provider_output = child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::TextReplace { text, .. } if text.contains("RESIDENT_STOP_TEXT")
        )
    });

    assert!(
        observed.is_ok(),
        "sidecar loss must end without the recovery transaction"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(claim_released);
    assert_eq!(supervisor.episode_count(), 0);
    assert_eq!(matching_failures, 1);
    assert_eq!(aborted_recovery_events, 0);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(child_events, child_replay_at_failure);
    assert!(!successful_provider_output);
    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    assert_stopped_member_revives(&engine, root, child, &handle, "after running sidecar loss")
        .await;
    supervisor.end_member(root, &handle).await.unwrap();
}

#[tokio::test]
async fn resident_running_loss_terminate_failure_disables_and_releases_claim() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls,
        entered: entered_tx,
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
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
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let loss = CancellationToken::new();
    let terminated = Arc::new(Notify::new());
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentTerminateFailureFactory {
        loss: loss.clone(),
        terminated: terminated.clone(),
        terminates: terminates.clone(),
    });
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, Some(factory)),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "running terminate failure".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");
    loss.cancel();

    let observed = tokio::time::timeout(RESIDENT_END_BOUND, async {
        terminated.notified().await;
        loop {
            let envelope = events.recv().await.unwrap();
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == &handle
                    && task.starts_with("activation sidecar lost")
            ) {
                break;
            }
        }
    })
    .await;
    release.notify_one();
    observed.expect("terminate failure must still finalize and release the resident");

    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(supervisor.episode_count(), 0);

    assert_sidecar_failed(&engine, &supervisor, root, child, &handle).await;
    let projection = engine.read_projection(root).await.unwrap();
    assert!(
        projection
            .session
            .members
            .iter()
            .find(|m| m.child == Some(child))
            .unwrap()
            .summary
            .contains("sidecar did not stop")
    );
    assert_stopped_member_revives(&engine, root, child, &handle, "after terminate failure").await;
    supervisor.end_member(root, &handle).await.unwrap();
}

#[tokio::test]
async fn governor_caps_streaming_concurrency() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ConcurrencyProbeProvider {
        current: current.clone(),
        peak: peak.clone(),
    });
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_concurrency: 2,
            per_run_budget: 100,
            ..SubagentLimits::default()
        },
        provider,
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let specs: Vec<MemberSpec> = (0..6)
        .map(|i| member(&engine, &agent, &format!("m{i}")))
        .collect();
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 6);
    assert!(evidence.iter().all(|e| e.status == MemberStatus::Done));
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "peak concurrent streams {} exceeded max_concurrency 2",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn governor_rejects_members_beyond_budget() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_concurrency: 8,
            per_run_budget: 1,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let specs = vec![
        member(&engine, &agent, "a"),
        member(&engine, &agent, "b"),
        member(&engine, &agent, "c"),
    ];
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 3);
    let done = evidence
        .iter()
        .filter(|e| e.status == MemberStatus::Done)
        .count();
    let failed = evidence
        .iter()
        .filter(|e| e.status == MemberStatus::Failed)
        .count();
    assert_eq!(done, 1, "only the budgeted member runs");
    assert_eq!(failed, 2, "the rest are rejected");
    assert!(
        evidence
            .iter()
            .any(|e| e.summary.contains("budget exhausted")),
        "rejected members explain the budget"
    );
}

#[tokio::test]
async fn governor_rejects_spawn_beyond_max_depth() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_concurrency: 8,
            per_run_budget: 100,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    // ADR-0015: depth is hardcoded to two layers. Build a depth-2 session;
    // its member would be depth 3 > MAX_SUBAGENT_DEPTH 2.
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let grandchild = engine
        .create(CreateSession {
            parent: Some(child),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let evidence = run_team(
        engine.clone(),
        grandchild,
        vec![member(&engine, &agent, "too deep")],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
    assert!(evidence[0].summary.contains("depth"));
}

#[tokio::test]
async fn run_team_records_member_lifecycle_on_lead() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let evidence = run_team(
        engine.clone(),
        lead,
        vec![
            member(&engine, &agent, "member one"),
            member(&engine, &agent, "member two"),
        ],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(evidence.len(), 2);

    // The lead projection now carries observable member lifecycle entries.
    let proj = engine.read_projection(lead).await.unwrap();
    assert_eq!(proj.session.members.len(), 2, "both members are tracked");
    assert!(
        proj.session
            .members
            .iter()
            .all(|m| matches!(m.status, hya_proto::MemberRunStatus::Done)),
        "members finished Done"
    );
    assert!(
        proj.session.members.iter().all(|m| m.child.is_some()),
        "each member links to its child session"
    );
}

#[tokio::test]
async fn transient_sidecar_shutdown_follows_member_finished() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let member_id = MemberId::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let member_finished_at_shutdown = Arc::new(AtomicBool::new(false));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ShutdownProbeFactory {
        engine: engine.clone(),
        lead,
        member: member_id,
        calls: calls.clone(),
        member_finished_at_shutdown: member_finished_at_shutdown.clone(),
    });
    let mut spec = member(&engine, &agent, "shutdown probe");
    spec.id = member_id;
    spec.sidecar_factory = Some(factory);

    let evidence = run_team(engine, lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(member_finished_at_shutdown.load(Ordering::SeqCst));
}

#[tokio::test]
async fn transient_sidecar_failure_terminates_opaque_handle_once() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy,
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        terminated: None,
        loss: None,
    });
    let mut failing_agent = agent;
    failing_agent.model = ModelRef::new("no-such-model");
    let mut spec = member(&engine, &failing_agent, "provider failure");
    spec.sidecar_factory = Some(factory);

    let evidence = run_team(engine, lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transient_sidecar_parent_cancellation_marks_member_cancelled_and_terminates_once() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicBool::new(false));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentStopProbeFactory {
        ready: ready.clone(),
        healthy,
        shutdowns: shutdowns.clone(),
        terminates: terminates.clone(),
        terminated: None,
        loss: None,
    });
    let mut spec = member(&engine, &agent, "cancelled before turn");
    let member_id = spec.id;
    spec.sidecar_factory = Some(factory);

    let cancel = CancellationToken::new();
    cancel.cancel();
    let evidence = run_team(engine.clone(), lead, vec![spec], cancel).await;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
    let events = engine.replay(lead).await.unwrap();
    let member_finished = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::MemberFinished { member, status, .. } if *member == member_id => Some(status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(member_finished.len(), 1);
    assert!(matches!(member_finished[0], MemberRunStatus::Cancelled));
    assert!(
        !member_finished
            .iter()
            .any(|status| matches!(status, MemberRunStatus::Done))
    );
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transient_sidecar_ack_failure_terminates_opaque_handle_once() {
    let polls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(AckProbeProvider {
        polls: polls.clone(),
        polled: Arc::new(Notify::new()),
    });
    let (engine, agent) = governed_engine(SubagentLimits::default(), provider).await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let terminates = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(AckFailureFactory {
        ready: ready.clone(),
        terminates: terminates.clone(),
        shutdowns: shutdowns.clone(),
    });
    let mut spec = member(&engine, &agent, "ACK failure");
    spec.sidecar_factory = Some(factory);

    let evidence = run_team(engine, lead, vec![spec], CancellationToken::new()).await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn team_evidence_envelope_has_no_transcript_leak() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let specs = vec![
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            binding: engine.bind_runtime(&agent.workdir).unwrap(),
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: "do A".to_string(),
            description: String::new(),
            session: None,
            sidecar_factory: None,
            tool_call: None,
        },
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            binding: engine.bind_runtime(&agent.workdir).unwrap(),
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: "do B".to_string(),
            description: String::new(),
            session: None,
            sidecar_factory: None,
            tool_call: None,
        },
    ];
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 2);
    assert!(evidence.iter().all(|e| e.status == MemberStatus::Done));

    project_envelope(&engine, lead, &TeamEvidenceEnvelope { members: evidence })
        .await
        .unwrap();

    let lead_proj = engine.read_projection(lead).await.unwrap();
    let has_envelope = lead_proj.session.messages.iter().any(|m| {
        matches!(m.role, Role::System)
            && m.parts.iter().any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains("TEAM EVIDENCE ENVELOPE")))
    });
    assert!(has_envelope, "lead must contain the evidence envelope");

    // The members ran in CHILD sessions: the lead transcript holds no replayed
    // assistant turns (no full-transcript leak into the lead context).
    let assistant_count = lead_proj
        .session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .count();
    assert_eq!(assistant_count, 0);
}

#[tokio::test]
async fn run_team_can_resume_existing_member_session() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            binding: engine.bind_runtime(&agent.workdir).unwrap(),
            agent,
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: "continue prior work".to_string(),
            description: String::new(),
            session: Some(child),
            sidecar_factory: None,
            tool_call: None,
        }],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].session, child.to_string());
    assert_eq!(evidence[0].status, MemberStatus::Done);
}

/// Restarting a failed/finished subagent via the same child session must not
/// grow the lead's member list or team roster. Duplicate members share one
/// session id and make the TUI roster multi-highlight on select.
#[tokio::test]
async fn run_team_resume_reuses_member_and_roster_handle() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let first = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            binding: engine.bind_runtime(&agent.workdir).unwrap(),
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: "first attempt".to_string(),
            description: String::new(),
            session: Some(child),
            sidecar_factory: None,
            tool_call: None,
        }],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, MemberStatus::Done);

    let after_first = engine.read_projection(lead).await.unwrap();
    let first_members: Vec<_> = after_first
        .session
        .members
        .iter()
        .filter(|m| m.child == Some(child))
        .collect();
    assert_eq!(first_members.len(), 1, "first spawn creates one member");
    let first_member_id = first_members[0].member;
    let first_handles: Vec<_> = after_first
        .team
        .roster
        .values()
        .filter(|e| e.session == child)
        .map(|e| e.handle.clone())
        .collect();
    assert_eq!(
        first_handles.len(),
        1,
        "first spawn creates one roster handle"
    );
    let first_handle = first_handles[0].clone();

    // Resume with a fresh MemberId (what the task tool does via task_id).
    let second = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            binding: engine.bind_runtime(&agent.workdir).unwrap(),
            agent,
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: "restart after failure".to_string(),
            description: String::new(),
            session: Some(child),
            sidecar_factory: None,
            tool_call: None,
        }],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].status, MemberStatus::Done);
    assert_eq!(second[0].session, child.to_string());

    let after_resume = engine.read_projection(lead).await.unwrap();
    let resume_members: Vec<_> = after_resume
        .session
        .members
        .iter()
        .filter(|m| m.child == Some(child))
        .collect();
    assert_eq!(
        resume_members.len(),
        1,
        "resume must not add a second member row for the same child session"
    );
    assert_eq!(
        resume_members[0].member, first_member_id,
        "resume should upsert the original member id"
    );

    let resume_handles: Vec<_> = after_resume
        .team
        .roster
        .values()
        .filter(|e| e.session == child)
        .map(|e| e.handle.clone())
        .collect();
    assert_eq!(
        resume_handles.len(),
        1,
        "resume must not allocate a second roster handle for the same session"
    );
    assert_eq!(resume_handles[0], first_handle);
}

#[tokio::test]
async fn panic_in_one_member_is_isolated() {
    let panicker = tokio::spawn(async { panic!("member exploded") });
    let peer = tokio::spawn(async { 7u32 });

    let joined = panicker.await;
    assert!(joined.is_err());
    assert!(joined.unwrap_err().is_panic());

    // the supervisor and peers survive a member panic
    assert_eq!(peer.await.unwrap(), 7);
}

#[tokio::test]
async fn run_team_marks_failed_member_without_session_on_engine_error() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let healthy_id = MemberId::new();
    let failed_id = MemberId::new();
    let mut failing_agent = agent.clone();
    failing_agent.model = ModelRef::new("no-such-model");

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![
            MemberSpec {
                id: healthy_id,
                agent: agent.clone(),
                binding: engine.bind_runtime(&agent.workdir).unwrap(),
                agents: Arc::from([]),
                resources: None,
                guidance: None,
                directive: "do healthy work".to_string(),
                description: String::new(),
                session: None,
                sidecar_factory: None,
                tool_call: None,
            },
            MemberSpec {
                id: failed_id,
                binding: engine.bind_runtime(&failing_agent.workdir).unwrap(),
                agent: failing_agent,
                agents: Arc::from([]),
                resources: None,
                guidance: None,
                directive: "do failing work".to_string(),
                description: String::new(),
                session: None,
                sidecar_factory: None,
                tool_call: None,
            },
        ],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(evidence.len(), 2);

    let healthy = evidence
        .iter()
        .find(|entry| entry.member == healthy_id.to_string())
        .unwrap();
    assert_eq!(healthy.status, MemberStatus::Done);

    let failed = evidence
        .iter()
        .find(|entry| entry.member == failed_id.to_string())
        .unwrap();
    assert_eq!(failed.status, MemberStatus::Failed);
    assert_eq!(failed.session, "-");
    assert!(!failed.summary.is_empty());
}

#[tokio::test]
async fn run_team_preserves_input_member_order_with_mixed_outcomes() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let first = MemberId::new();
    let second = MemberId::new();
    let third = MemberId::new();
    let mut failing_agent = agent.clone();
    failing_agent.model = ModelRef::new("no-such-model");

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![
            MemberSpec {
                id: first,
                agent: agent.clone(),
                binding: engine.bind_runtime(&agent.workdir).unwrap(),
                agents: Arc::from([]),
                resources: None,
                guidance: None,
                directive: "first member".to_string(),
                description: String::new(),
                session: None,
                sidecar_factory: None,
                tool_call: None,
            },
            MemberSpec {
                id: second,
                binding: engine.bind_runtime(&failing_agent.workdir).unwrap(),
                agent: failing_agent,
                agents: Arc::from([]),
                resources: None,
                guidance: None,
                directive: "second member fails".to_string(),
                description: String::new(),
                session: None,
                sidecar_factory: None,
                tool_call: None,
            },
            MemberSpec {
                id: third,
                binding: engine.bind_runtime(&agent.workdir).unwrap(),
                agent,
                agents: Arc::from([]),
                resources: None,
                guidance: None,
                directive: "third member".to_string(),
                description: String::new(),
                session: None,
                sidecar_factory: None,
                tool_call: None,
            },
        ],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        evidence
            .iter()
            .map(|entry| entry.member.clone())
            .collect::<Vec<_>>(),
        vec![first.to_string(), second.to_string(), third.to_string()]
    );
    assert_eq!(
        evidence
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![MemberStatus::Done, MemberStatus::Failed, MemberStatus::Done]
    );
}

/// Wrap one prepared AgentBundle as an agent catalog over the compiled-in built-ins.
fn agent_catalog(bundle: Bundle) -> Result<AgentCatalog, hya_bundle::BundleError> {
    let bundles = [bundle];
    AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&bundles)?))
}
