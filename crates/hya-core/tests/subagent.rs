//! Integration tests for `hya-core`: subagent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use async_trait::async_trait;
use futures::{FutureExt as _, stream};
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, PreparedResource, ResourceView, SpawnLifecycle,
};
use hya_core::{
    AdmissionMemberIdentity, AgentSpec, BoundSidecarFactory, BoundSpawnSender, ChatParamsInput,
    ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, CoreError,
    CreateSession, EventBus, HookDispatcher, MemberSpec, MemberStatus, MessageUserBeforeInput,
    MessageUserBeforeOutcome, ResidentSupervisor, RuntimeRegistry, SessionEngine, SidecarHandle,
    SidecarLifecycle, SidecarStart, SubagentGovernor, SubagentLimits, TeamEvidenceEnvelope,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, project_envelope, run_pre_admitted_member,
    run_team,
};
use hya_proto::{
    AgentName, Event, FinishReason, MailEndpoint, MailKind, MemberId, MemberRunStatus, MessageId,
    ModelRef, PartProjection, Role, RosterStatus, SessionId, ToolName, ToolSchema,
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
            parallel_tool_calls: true,
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
            parallel_tool_calls: true,
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
            parallel_tool_calls: true,
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
            parallel_tool_calls: true,
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
        self.entered.notify_one();
        let release = self.release.clone();
        Ok(Box::pin(stream::once(async move {
            release.notified().await;
            Ok::<Event, ProviderError>(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
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

struct AckFailureRaceFactory {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

struct AckFailureRaceHandle {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for AckFailureRaceFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(AckFailureRaceHandle {
            entered: self.entered.clone(),
            release: self.release.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for AckFailureRaceHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Err(CoreError::Invalid("sidecar ACK rejected".to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminates.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
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

fn sidecar_permission_bundle(spawn_lifecycle: SpawnLifecycle) -> PreparedBundle {
    PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/sidecar-permission".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: vec![PreparedAgent {
            local_id: "sidecar-agent".to_string(),
            stable_id: AgentName::new("sidecar-agent"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some("sidecar permission prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle,
            harness_access: HarnessAccess::Full,
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
            aliases: Vec::new(),
        }],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
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

struct ResidentBudgetTerminateRetryFactory {
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

struct ResidentBudgetTerminateRetryHandle {
    terminated: Arc<Notify>,
    terminates: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for ResidentBudgetTerminateRetryFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(ResidentBudgetTerminateRetryHandle {
            terminated: self.terminated.clone(),
            terminates: self.terminates.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for ResidentBudgetTerminateRetryHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        let attempt = self.terminates.fetch_add(1, Ordering::SeqCst) + 1;
        self.terminated.notify_one();
        if attempt == 1 {
            Err(CoreError::Invalid(
                "test resident terminate failure".to_string(),
            ))
        } else {
            Ok(())
        }
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

struct CleanupRetryFactory {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    loss: CancellationToken,
    shutdown_calls: Arc<AtomicUsize>,
    terminate_calls: Arc<AtomicUsize>,
}

struct CleanupRetryHandle {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    loss: CancellationToken,
    shutdown_calls: Arc<AtomicUsize>,
    terminate_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BoundSidecarFactory for CleanupRetryFactory {
    async fn start(&self, _start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(CleanupRetryHandle {
            entered: self.entered.clone(),
            release: self.release.clone(),
            loss: self.loss.clone(),
            shutdown_calls: self.shutdown_calls.clone(),
            terminate_calls: self.terminate_calls.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for CleanupRetryHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        let attempt = self.shutdown_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
            self.entered.notify_one();
            self.release.notified().await;
            Err(CoreError::Invalid("stop cleanup failed".to_string()))
        } else {
            Ok(())
        }
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminate_calls.fetch_add(1, Ordering::SeqCst);
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
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

#[tokio::test]
async fn pre_admitted_member_nested_spawn_carries_parent_admission_identity() {
    let workdir = support::TestDir::new("pre-admitted-nested-spawn");
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "task".to_string(),
                input: json!({
                    "description": "nested task",
                    "prompt": "nested prompt",
                    "subagent_type": "general"
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "member harness".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };
    let agent = engine
        .agent_spec_for_binding(&binding, &base, "general")
        .unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "general")
        .unwrap();
    let resources = binding.agent_resource_policy("general").unwrap();
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
    assert_eq!(bound.parent_admission(), Some(admission));
    let (_binding, nested_request) = bound.into_parts();
    let nested_parent = nested_request.parent;
    nested_request
        .reply
        .send(Ok(vec![MemberOutcome {
            member: "nested-member".to_string(),
            session: SessionId::new().to_string(),
            status: "done".to_string(),
            summary: "nested complete".to_string(),
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
            parallel_tool_calls: true,
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
        name: AgentName::new("build"),
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
    let counts = engine.store().admission_counts().await.unwrap();
    assert_eq!(counts.active, 0);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 0);

    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
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
    let counts = engine.store().admission_counts().await.unwrap();
    assert_eq!(counts.active, 0);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 0);

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

    let counts = engine.store().admission_counts().await.unwrap();
    assert_eq!(counts.active, 0);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 0);
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
    let catalog = Arc::new(
        BundleCatalog::from_prepared(&[sidecar_permission_bundle(SpawnLifecycle::Transient)])
            .unwrap(),
    );
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
    let catalog = Arc::new(
        BundleCatalog::from_prepared(&[sidecar_permission_bundle(SpawnLifecycle::Transient)])
            .unwrap(),
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

#[tokio::test]
async fn resident_sidecar_tool_binding_reaches_captured_turn_view() {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(
        BundleCatalog::from_prepared(&[sidecar_permission_bundle(SpawnLifecycle::Resident)])
            .unwrap(),
    );
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
        projection.team.roster.get(&handle).unwrap().status,
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

    supervisor.stop_resident(root, &handle).await.unwrap();
    supervisor.stop_resident(root, &handle).await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );

    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    assert_eq!(entry.current_task.as_deref(), Some("resident stopped"));
    assert!(supervisor.team_cancel(root).is_none());

    let error = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "after stop".to_string(),
        )
        .await
        .expect_err("stopped resident must reject new mail");
    assert!(matches!(
        error,
        CoreError::Store(hya_store::StoreError::MailboxRejected(message))
            if message.contains("stopped") || message.contains("terminal")
    ));
}

#[tokio::test]
async fn duplicate_stop_shares_cleanup_failure_and_later_retries_cleanup_without_reterminalizing() {
    let (engine, agent) = engine().await;
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let loss = CancellationToken::new();
    let shutdown_calls = Arc::new(AtomicUsize::new(0));
    let terminate_calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(CleanupRetryFactory {
        entered: entered.clone(),
        release: release.clone(),
        loss: loss.clone(),
        shutdown_calls: shutdown_calls.clone(),
        terminate_calls: terminate_calls.clone(),
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
            "activate cleanup retry sidecar".to_string(),
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

    let first_supervisor = supervisor.clone();
    let first_handle = handle.clone();
    let first_stop =
        tokio::spawn(async move { first_supervisor.stop_resident(root, &first_handle).await });
    entered.notified().await;

    let mut duplicate_stop = Box::pin(supervisor.stop_resident(root, &handle));
    assert!(
        duplicate_stop.as_mut().now_or_never().is_none(),
        "duplicate stop must remain pending until cleanup completes"
    );
    release.notify_one();

    let first_result = first_stop.await.unwrap();
    let duplicate_result = duplicate_stop.await;
    loss.cancel();
    let retry_result = supervisor.stop_resident(root, &handle).await;
    assert_eq!(shutdown_calls.load(Ordering::SeqCst), 2);
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 0);
    assert!(
        retry_result.is_ok(),
        "cleanup retry must succeed: {retry_result:?}"
    );
    assert!(first_result.is_err());
    assert!(duplicate_result.is_err());

    let first_error = first_result.expect_err("first stop must report cleanup failure");
    let duplicate_error = duplicate_result.expect_err("duplicate stop must report cleanup failure");
    assert!(matches!(
        &first_error,
        CoreError::Invalid(message) if message == "stop cleanup failed"
    ));
    assert!(matches!(
        &duplicate_error,
        CoreError::Invalid(message) if message == "stop cleanup failed"
    ));
    assert_eq!(first_error.to_string(), duplicate_error.to_string());

    let stop_events = engine
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
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(stop_events, 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_none());
}

#[tokio::test]
async fn failed_running_stop_cleanup_cannot_become_ok_after_team_kill() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let provider = Arc::new(ResidentStopBarrierProvider {
        polls,
        entered: entered_tx,
        release: release.clone(),
    });
    let (engine, agent) = governed_engine(
        SubagentLimits {
            per_team_message_budget: 1,
            ..SubagentLimits::default()
        },
        provider,
    )
    .await;
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let loss = CancellationToken::new();
    let terminated = Arc::new(Notify::new());
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentTerminateFailureFactory {
        loss,
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
            "running stop cleanup".to_string(),
        )
        .await
        .unwrap();
    entered_rx
        .recv()
        .await
        .expect("provider must enter first turn");

    let first_supervisor = supervisor.clone();
    let first_handle = handle.clone();
    let first_stop =
        tokio::spawn(async move { first_supervisor.stop_resident(root, &first_handle).await });
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
                && task == "resident stopped"
        ) {
            break;
        }
    }

    release.notify_one();
    terminated.notified().await;
    let first_result = first_stop.await.unwrap();
    assert!(matches!(
        &first_result,
        Err(CoreError::Invalid(message)) if message == "test resident terminate failure"
    ));

    engine
        .mail_send(
            root,
            MailEndpoint::Channel("trip".to_string()),
            MailKind::Message,
            "budget trip".to_string(),
        )
        .await
        .unwrap();
    terminated.notified().await;
    tokio::task::yield_now().await;

    assert!(supervisor.team_cancel(root).is_some());
    let second_result = supervisor.stop_resident(root, &handle).await;
    assert!(matches!(
        &second_result,
        Err(CoreError::Invalid(message)) if message == "test resident terminate failure"
    ));
    assert_eq!(terminates.load(Ordering::SeqCst), 3);
    assert!(supervisor.team_cancel(root).is_some());

    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    assert_eq!(entry.current_task.as_deref(), Some("resident stopped"));
    let stop_events = engine
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
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(stop_events, 1);
}

#[tokio::test]
async fn resident_stop_durable_failure_defers_cleanup_and_allows_retry() {
    let db_dir = support::TestDir::new("resident-stop-durable-failure");
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
        name: AgentName::new("build"),
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
            "resident stop durable failure".to_string(),
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

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_resident_stop_durable_failure \
         BEFORE UPDATE OF state ON resident_actor_claim \
         WHEN NEW.state = 'released' AND OLD.state = 'active' \
         BEGIN SELECT RAISE(ABORT, 'test stop boundary'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();

    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await
        .unwrap();
    let first_supervisor = supervisor.clone();
    let first_handle = handle.clone();
    let mut first_stop = Box::pin(first_supervisor.stop_resident(root, &first_handle));
    assert!(
        first_stop.as_mut().now_or_never().is_none(),
        "leader stop must remain pending behind the writer"
    );
    let duplicate_supervisor = supervisor.clone();
    let duplicate_handle = handle.clone();
    let mut duplicate_stop = Box::pin(duplicate_supervisor.stop_resident(root, &duplicate_handle));
    assert!(
        duplicate_stop.as_mut().now_or_never().is_none(),
        "duplicate stop must await the shared completion"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut connection)
        .await
        .unwrap();
    let first_result = first_stop.await;
    let duplicate_result = duplicate_stop.await;
    let first_error = first_result.expect_err("leader stop must report the SQLx failure");
    let duplicate_error =
        duplicate_result.expect_err("duplicate stop must report the SQLx failure");
    assert!(matches!(
        &first_error,
        CoreError::Store(hya_store::StoreError::Sqlite(_))
    ));
    assert!(matches!(
        &duplicate_error,
        CoreError::Store(hya_store::StoreError::Sqlite(_))
    ));
    assert_eq!(first_error.to_string(), duplicate_error.to_string());
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
    assert!(supervisor.team_cancel(root).is_some());
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let stop_events = engine
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
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(stop_events, 0);

    sqlx::query("DROP TRIGGER test_resident_stop_durable_failure")
        .execute(&mut connection)
        .await
        .unwrap();
    let second_result = supervisor.stop_resident(root, &handle).await;
    assert!(second_result.is_ok());
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 0);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let stop_events = engine
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
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(stop_events, 1);
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
        name: AgentName::new("build"),
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
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
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
    let result = supervisor.stop_resident(root, &handle).await;
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
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Idle);
    assert_ne!(entry.current_task.as_deref(), Some("resident stopped"));
}

#[tokio::test]
async fn resident_direct_send_committed_before_stop_is_durably_cancelled() {
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
        name: AgentName::new("build"),
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
        before_send.team.roster.get(&handle).unwrap().status,
        RosterStatus::Idle
    );

    let sender = make_engine(
        SessionStore::connect(&db_path).await.unwrap(),
        EventBus::default(),
    );
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

    let stop_result = supervisor.stop_resident(root, &handle).await;
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
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    let inbox_len = projection.team.inboxes.get(&handle).map_or(0, Vec::len);
    assert_eq!(inbox_len, 1);
    assert_eq!(entry.resident_cursor, inbox_len as u64);
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
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    assert!(
        entry
            .current_task
            .as_deref()
            .is_some_and(|task| task.contains("message budget exceeded"))
    );
    assert!(supervisor.team_cancel(root).is_none());
}

#[tokio::test]
async fn message_budget_kill_terminate_failure_allows_explicit_cleanup_retry() {
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
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let terminated = Arc::new(Notify::new());
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(ResidentBudgetTerminateRetryFactory {
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

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "budget trip".to_string(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(250), async {
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
    })
    .await
    .expect("message budget failure event must be emitted");
    tokio::time::timeout(std::time::Duration::from_millis(250), terminated.notified())
        .await
        .expect("resident sidecar termination must be attempted after team kill");

    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_some());
    let budget_failed_before = engine
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
                    && task.contains("message budget exceeded")
            )
        })
        .count();
    assert_eq!(budget_failed_before, 1);

    let stop_result = supervisor.stop_resident(root, &handle).await;
    assert!(
        stop_result.is_ok(),
        "explicit cleanup retry must succeed: {stop_result:?}"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 2);
    assert!(supervisor.team_cancel(root).is_none());
    let events_after_stop = engine.store().replay(root).await.unwrap();
    let budget_failed_after = events_after_stop
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
                    && task.contains("message budget exceeded")
            )
        })
        .count();
    assert_eq!(budget_failed_after, 1);
    let failed_terminal_count = events_after_stop
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    ..
                } if *session == root && event_handle == &handle
            )
        })
        .count();
    assert_eq!(failed_terminal_count, 1);

    let repeat_result = supervisor.stop_resident(root, &handle).await;
    assert!(repeat_result.is_ok(), "repeat stop must remain idempotent");
    assert_eq!(terminates.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn message_budget_kill_store_failure_preserves_claim_and_slot_for_retry() {
    let db_dir = support::TestDir::new("message-budget-kill-store-failure");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
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
        .with_governor(SubagentGovernor::new(SubagentLimits {
            per_team_message_budget: 1,
            ..SubagentLimits::default()
        })),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let root_a = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let root_b = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let binding_a = engine.bind_runtime(&agent.workdir).unwrap();
    let resources_a = binding_a
        .agent_resource_policy(agent.name.as_str())
        .unwrap();
    let (child_a, handle_a) = supervisor
        .spawn_resident(
            root_a,
            agent.clone(),
            (binding_a, Arc::from([]), resources_a, None),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();
    let binding_b = engine.bind_runtime(&agent.workdir).unwrap();
    let resources_b = binding_b
        .agent_resource_policy(agent.name.as_str())
        .unwrap();
    let (child_b, handle_b) = supervisor
        .spawn_resident(
            root_b,
            agent,
            (binding_b, Arc::from([]), resources_b, None),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root_a,
            MailEndpoint::Handle(handle_a.clone()),
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
                handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root_a && handle == &handle_a
        ) {
            break;
        }
    }

    let root_a_cancel = supervisor
        .team_cancel(root_a)
        .expect("resident team must have a cancellation token");
    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_message_budget_kill_store_failure \
         BEFORE UPDATE OF state ON resident_actor_claim \
         WHEN NEW.state = 'released' AND OLD.state = 'active' \
         BEGIN SELECT RAISE(ABORT, 'test message budget kill release failure'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();

    engine
        .mail_send(
            root_a,
            MailEndpoint::Handle(handle_a.clone()),
            MailKind::Message,
            "budget trip".to_string(),
        )
        .await
        .unwrap();
    root_a_cancel.cancelled().await;

    engine
        .mail_send(
            root_b,
            MailEndpoint::Handle(handle_b.clone()),
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
                handle,
                status: RosterStatus::Idle,
                ..
            } if *session == root_b && handle == &handle_b
        ) {
            break;
        }
    }

    let budget_failed_before_retry = engine
        .store()
        .replay(root_a)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root_a
                    && handle == &handle_a
                    && task.contains("message budget exceeded")
            )
        })
        .count();
    assert_eq!(budget_failed_before_retry, 0);
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child_a)
    );
    assert!(supervisor.team_cancel(root_a).is_some());

    sqlx::query("DROP TRIGGER test_message_budget_kill_store_failure")
        .execute(&mut connection)
        .await
        .unwrap();
    let stop_result = supervisor.stop_resident(root_a, &handle_a).await;
    assert!(
        stop_result.is_ok(),
        "explicit cleanup retry must succeed: {stop_result:?}"
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child_a)
    );
    assert!(supervisor.team_cancel(root_a).is_none());
    let events_after_stop = engine.store().replay(root_a).await.unwrap();
    let failed_terminal_count = events_after_stop
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle,
                    status: RosterStatus::Failed,
                    ..
                } if *session == root_a && handle == &handle_a
            )
        })
        .count();
    assert_eq!(failed_terminal_count, 1);

    supervisor.stop_resident(root_b, &handle_b).await.unwrap();
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child_b)
    );
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
        tokio::spawn(async move { first_supervisor.stop_resident(root, &first_handle).await });
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
                && task == "resident stopped"
        ) {
            break;
        }
    }

    let mut second_stop = Box::pin(supervisor.stop_resident(root, &handle));
    assert!(
        second_stop.as_mut().now_or_never().is_none(),
        "duplicate stop must remain pending until the leader's durable finalization plus cleanup completes"
    );
    let replay_at_recovery = engine.store().replay(child).await.unwrap().len();
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
    assert!(supervisor.team_cancel(root).is_none());

    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(!child_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            matches!(
                part,
                PartProjection::Text { text, .. } if text.contains("queued after stop")
            )
        })
    }));
    assert_eq!(
        engine.store().replay(child).await.unwrap().len(),
        replay_at_recovery
    );

    let error = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "after stop".to_string(),
        )
        .await
        .expect_err("stopped resident must reject new mail");
    assert!(matches!(
        error,
        CoreError::Store(hya_store::StoreError::MailboxRejected(message))
            if message.contains("stopped") || message.contains("terminal")
    ));
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
    assert!(supervisor.team_cancel(root).is_none());

    let third = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident declaration drift third".to_string(),
        )
        .await;
    assert!(matches!(
        third,
        Err(CoreError::Store(hya_store::StoreError::MailboxRejected(message)))
            if message.contains("stopped") || message.contains("terminal")
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(!child_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            matches!(
                part,
                PartProjection::Text { text, .. }
                    if text.contains("declaration drift second")
                        || text.contains("declaration drift third")
            )
        })
    }));
}

#[tokio::test]
async fn resident_activation_hook_transport_loss_enters_epoch_recovery() {
    assert_resident_hook_transport_loss(HookLossStage::Before).await;
}

async fn assert_resident_hook_transport_loss(stage: HookLossStage) {
    let canonical = SIDECAR_PERMISSION_TOOL;
    let catalog = Arc::new(
        BundleCatalog::from_prepared(&[sidecar_permission_bundle(SpawnLifecycle::Resident)])
            .unwrap(),
    );
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
    supervisor.team_cancel(root).unwrap().cancel();

    assert_eq!(observed_status, RosterStatus::Failed);
    assert_eq!(
        observed_task.as_deref(),
        Some("aborted by resident recovery")
    );
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
    supervisor
        .register_existing_resident_with_sidecar(
            root,
            child,
            "worker-1".to_string(),
            agent.clone(),
            None,
            factory,
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
            MailEndpoint::Handle("worker-1".to_string()),
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
    let worker = root_projection.team.roster.get("worker-1").unwrap();
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
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
    supervisor
        .register_existing_resident_with_sidecar(
            root,
            child,
            "worker-1".to_string(),
            agent,
            None,
            factory,
        )
        .await
        .unwrap();

    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("worker-1".to_string()),
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
                && handle == "worker-1"
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
    assert!(supervisor.team_cancel(root).is_none());
}

#[tokio::test]
async fn resident_sidecar_ready_failure_finalize_rollback_keeps_slot_for_retry() {
    let db_dir = support::TestDir::new("resident-sidecar-ready-failure-finalize-rollback");
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
        name: AgentName::new("build"),
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
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
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
    let handle = "worker-1".to_string();
    supervisor
        .register_existing_resident_with_sidecar(root, child, handle.clone(), agent, None, factory)
        .await
        .unwrap();

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_resident_sidecar_ready_failure_finalize_rollback \
         BEFORE UPDATE OF state ON resident_actor_claim \
         WHEN NEW.state = 'released' AND OLD.state = 'active' \
         BEGIN SELECT RAISE(ABORT, 'test sidecar finalize boundary'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident task".to_string(),
        )
        .await
        .unwrap();
    let activation_attempt = tokio::time::timeout(std::time::Duration::from_millis(250), async {
        while ready.load(Ordering::SeqCst) != 1 || terminates.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        activation_attempt.is_ok(),
        "resident sidecar ACK failure attempt must complete"
    );

    assert_eq!(ready.load(Ordering::SeqCst), 1);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_some());
    let failed_before_retry = engine
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
                    ..
                } if *session == root && event_handle == &handle
            )
        })
        .count();
    assert_eq!(failed_before_retry, 0);

    sqlx::query("DROP TRIGGER test_resident_sidecar_ready_failure_finalize_rollback")
        .execute(&mut connection)
        .await
        .unwrap();
    let stop_result = supervisor.stop_resident(root, &handle).await;
    assert!(
        stop_result.is_ok(),
        "resident stop must retry: {stop_result:?}"
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_none());
    let failed_after_retry = engine
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
                    ..
                } if *session == root && event_handle == &handle
            )
        })
        .count();
    assert_eq!(failed_after_retry, 1);
}

#[tokio::test]
async fn resident_stop_concurrent_with_ready_failure_cleanup_completes_idempotently() {
    let db_dir = support::TestDir::new("resident-sidecar-ready-failure-stop-race");
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
        name: AgentName::new("build"),
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
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let terminates = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(AckFailureRaceFactory {
        entered: entered.clone(),
        release: release.clone(),
        terminates: terminates.clone(),
    });
    let handle = "worker-1".to_string();
    supervisor
        .register_existing_resident_with_sidecar(root, child, handle.clone(), agent, None, factory)
        .await
        .unwrap();

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident task".to_string(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(250), entered.notified())
        .await
        .expect("resident sidecar ACK failure must enter termination");
    assert_eq!(terminates.load(Ordering::SeqCst), 1);

    let stop_supervisor = supervisor.clone();
    let stop_handle = handle.clone();
    let stop_task =
        tokio::spawn(async move { stop_supervisor.stop_resident(root, &stop_handle).await });
    tokio::time::timeout(std::time::Duration::from_millis(250), async {
        loop {
            if !engine
                .store()
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&child)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stop writer transaction must release the resident claim");
    assert!(
        supervisor.team_cancel(root).is_some(),
        "resident slot must remain while activation cleanup is blocked"
    );

    release.notify_one();
    let stop_result = stop_task.await.unwrap();
    assert!(
        stop_result.is_ok(),
        "concurrent stop must complete after activation cleanup: {stop_result:?}"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_none());

    let failed = engine
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
                    ..
                } if *session == root && event_handle == &handle
            )
        })
        .count();
    assert_eq!(failed, 1);
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
async fn resident_prework_sidecar_loss_rearms_queued_mail_under_recovered_claim() {
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
            "rearm after prework loss".to_string(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(250), terminated.notified())
        .await
        .expect("first prework sidecar loss must terminate deterministically");

    let polled_result =
        tokio::time::timeout(std::time::Duration::from_millis(250), polled.notified()).await;
    assert!(
        polled_result.is_ok(),
        "queued mail must re-arm a fresh sidecar and poll the provider"
    );
    tokio::time::timeout(std::time::Duration::from_millis(250), async {
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
    })
    .await
    .expect("recovered resident must become idle before explicit cleanup");
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(supervisor.team_cancel(root).is_some());

    supervisor.stop_resident(root, &handle).await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
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

    let observed = tokio::time::timeout(std::time::Duration::from_millis(250), async {
        let mut terminated_seen = false;
        let mut recovery_seen = false;
        while !(terminated_seen && recovery_seen) {
            tokio::select! {
                _ = terminated.notified(), if !terminated_seen => terminated_seen = true,
                envelope = events.recv(), if !recovery_seen => {
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
                            && task == "aborted by resident recovery"
                    ) {
                        recovery_seen = true;
                    }
                }
            }
        }
    })
    .await;
    let terminated_before_release = terminates.load(Ordering::SeqCst);
    let slot_remains_before_release = supervisor.team_cancel(root).is_some();
    let claim_remains_before_release = engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&child);

    release.notify_one();
    let _ = supervisor.stop_resident(root, &handle).await;
    let child_events = engine.store().replay(child).await.unwrap();
    let successful_provider_output = child_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::TextReplace { text, .. } if text.contains("RESIDENT_STOP_TEXT")
        )
    });

    assert!(
        observed.is_ok(),
        "running resident sidecar loss must recover before provider release"
    );
    assert_eq!(terminated_before_release, 1);
    assert!(slot_remains_before_release);
    assert!(claim_remains_before_release);
    assert!(!successful_provider_output);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resident_running_loss_recover_claim_failure_finalizes_old_claim() {
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
        name: AgentName::new("build"),
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

    let observed = tokio::time::timeout(std::time::Duration::from_millis(250), async {
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
                            && task.starts_with("resident recovery failed")
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
                    && task.starts_with("resident recovery failed")
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
        "recover_claim failure must terminate and finalize the resident"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(claim_released);
    assert!(supervisor.team_cancel(root).is_none());
    assert_eq!(matching_failures, 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(!successful_provider_output);
}

#[tokio::test]
async fn resident_running_loss_recovery_transaction_failure_finalizes_recovered_claim() {
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
        name: AgentName::new("build"),
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

    let observed = tokio::time::timeout(std::time::Duration::from_millis(250), async {
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
                            && task.starts_with("resident recovery failed")
                    ) {
                        failure_seen = true;
                    }
                }
            }
        }
    })
    .await;

    let child_replay_at_recovery = engine.store().replay(child).await.unwrap();
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
                    && task.starts_with("resident recovery failed")
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
        "recovery transaction failure must terminate and finalize the recovered resident"
    );
    assert_eq!(terminates.load(Ordering::SeqCst), 1);
    assert!(claim_released);
    assert!(supervisor.team_cancel(root).is_none());
    assert_eq!(matching_failures, 1);
    assert_eq!(aborted_recovery_events, 0);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(child_events, child_replay_at_recovery);
    assert!(!successful_provider_output);
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

    let observed = tokio::time::timeout(std::time::Duration::from_millis(250), async {
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
                    && task == "resident stopped"
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
    assert!(supervisor.team_cancel(root).is_none());

    let error = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "after terminate failure".to_string(),
        )
        .await
        .expect_err("disabled resident must reject new mail");
    assert!(matches!(
        error,
        CoreError::Store(hya_store::StoreError::MailboxRejected(message))
            if message.contains("stopped") || message.contains("terminal")
    ));
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
            max_depth: 5,
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            max_depth: 5,
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            max_depth: 1,
            max_concurrency: 8,
            per_run_budget: 100,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    // A depth-1 child; its member would be depth 2 > max_depth 1.
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let evidence = run_team(
        engine.clone(),
        child,
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
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
