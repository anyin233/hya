//! Native bundle extension traits registered through ABI v2.
//!
//! Alongside [`hya_tool::Tool`] and [`hya_context::rewrite::Rewriter`], native
//! bundles may register lifecycle services, runtime hooks, bundle HTTP/gRPC API
//! handlers, event projection reducers, custom URI scheme handlers, and
//! drivers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_proto::{
    ActorClaim, AgentName, Envelope, EventSeq, FinishCause, FinishReason, MessageId, ModelRef,
    PartId, SessionId, ToolCallId,
};
use hya_tool::{Action, Decision, Resource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::HostError;
use crate::host::HostRuntime;

// ---------------------------------------------------------------------------
// NativeService
// ---------------------------------------------------------------------------

/// Health snapshot reported by a [`NativeService`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceHealth {
    /// Whether the service is healthy and ready to serve requests.
    pub healthy: bool,
    /// Short human-readable status label (e.g. `"ready"`, `"starting"`, `"stopped"`).
    pub status: String,
    /// Optional structured diagnostic details.
    pub details: Option<Value>,
}

impl ServiceHealth {
    /// Construct a healthy status snapshot.
    #[must_use]
    pub fn healthy(status: impl Into<String>) -> Self {
        Self {
            healthy: true,
            status: status.into(),
            details: None,
        }
    }

    /// Construct an unhealthy status snapshot.
    #[must_use]
    pub fn unhealthy(status: impl Into<String>) -> Self {
        Self {
            healthy: false,
            status: status.into(),
            details: None,
        }
    }
}

/// Long-lived background service managed by the host lifecycle.
#[async_trait]
pub trait NativeService: Send + Sync {
    /// Stable service identifier within its bundle (e.g. `"lsp"`, `"supervisor"`).
    fn id(&self) -> &str;

    /// Start the service with access to the host runtime.
    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError>;

    /// Gracefully stop the service and release any background tasks.
    async fn stop(&self) -> Result<(), HostError>;

    /// Query current service health.
    async fn health(&self) -> ServiceHealth;

    /// Return `true` if this service currently owns active background work on `session`.
    fn is_session_busy(&self, _session: SessionId) -> bool {
        false
    }

    /// Request cancellation of any active service work on `session`.
    ///
    /// Returns `true` if an active operation on `session` was signalled.
    fn cancel_session(&self, _session: SessionId) -> bool {
        false
    }

    /// Wait until any active service work on `session` has fully stopped.
    async fn wait_session_idle(&self, _session: SessionId) -> Result<(), HostError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NativeHook
// ---------------------------------------------------------------------------

/// Named hook interception points supported by [`NativeHook`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    /// Intercept or veto tool arguments before execution (`tool.execute.before`).
    ToolExecuteBefore,
    /// Rewrite tool results or errors after execution (`tool.execute.after`).
    ToolExecuteAfter,
    /// Filter or adjust advertised tools for a turn (`tool.visibility`).
    ToolVisibility,
    /// Intercept or supply instructions before compaction (`compaction.before`).
    CompactionBefore,
    /// Observe committed compaction summary size (`compaction.after`).
    CompactionAfter,
    /// Observe session creation (`session.start`).
    SessionStart,
    /// Observe session closure/archive (`session.end`).
    SessionEnd,
    /// Observe child subagent registration (`agent.spawn`).
    AgentSpawn,
    /// Rewrite shell command text before execution (`command.execute.before`).
    CommandExecuteBefore,
    /// Rewrite user prompt text before admission (`message.user.before`).
    MessageUserBefore,
    /// Consulted before running a turn (`turn.admit`).
    TurnAdmit,
    /// Rewrite completed assistant text (`text.complete`).
    TextComplete,
    /// Adjust completion parameters before provider call (`chat.params`).
    ChatParams,
    /// Choose fallback model on pre-stream provider failure (`model.fallback`).
    ModelFallback,
    /// Intercept permission ask (`permission.ask`).
    PermissionAsk,
    /// Approve under a bundle-declared permission mode (`permission.approve`).
    PermissionApprove,
    /// Observe live envelopes published on the event bus (`event`).
    EventObserve,
    /// Hold a guard for each model stream a turn opens (`model.stream.permit`).
    ModelStreamPermit,
    /// Observe a user stop before the host cancels the turn (`turn.stop`).
    TurnStop,
    /// Approve or refuse archiving or deleting a session tree (`session.close.before`).
    SessionClose,
    /// Report pending work around a headless run (`run.headless`).
    HeadlessRun,
    /// Add text to a session's restore prompt (`session.restore`).
    SessionRestore,
    /// Observe that every restore prompt was admitted (`runtime.restored`).
    RuntimeRestored,
}

impl HookPoint {
    /// Every core hook point, in dispatch-table order.
    pub const ALL: [Self; 23] = [
        Self::ToolExecuteBefore,
        Self::ToolExecuteAfter,
        Self::ToolVisibility,
        Self::CompactionBefore,
        Self::CompactionAfter,
        Self::SessionStart,
        Self::SessionEnd,
        Self::AgentSpawn,
        Self::CommandExecuteBefore,
        Self::MessageUserBefore,
        Self::TurnAdmit,
        Self::TextComplete,
        Self::ChatParams,
        Self::ModelFallback,
        Self::PermissionAsk,
        Self::PermissionApprove,
        Self::EventObserve,
        Self::ModelStreamPermit,
        Self::TurnStop,
        Self::SessionClose,
        Self::HeadlessRun,
        Self::SessionRestore,
        Self::RuntimeRestored,
    ];

    /// Hook name used by bundle manifests and, where supported, the plugin wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ToolExecuteBefore => "tool.execute.before",
            Self::ToolExecuteAfter => "tool.execute.after",
            Self::ToolVisibility => "tool.visibility",
            Self::CompactionBefore => "compaction.before",
            Self::CompactionAfter => "compaction.after",
            Self::SessionStart => "session.start",
            Self::SessionEnd => "session.end",
            Self::AgentSpawn => "agent.spawn",
            Self::CommandExecuteBefore => "command.execute.before",
            Self::MessageUserBefore => "message.user.before",
            Self::TurnAdmit => "turn.admit",
            Self::TextComplete => "experimental.text.complete",
            Self::ChatParams => "chat.params",
            Self::ModelFallback => "model.fallback",
            Self::PermissionAsk => "permission.ask",
            Self::PermissionApprove => "permission.approve",
            Self::EventObserve => "event",
            Self::ModelStreamPermit => "model.stream.permit",
            Self::TurnStop => "turn.stop",
            Self::SessionClose => "session.close.before",
            Self::HeadlessRun => "run.headless",
            Self::SessionRestore => "session.restore",
            Self::RuntimeRestored => "runtime.restored",
        }
    }

    /// Parse a hook name produced by [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.name() == name)
    }
}

/// Whether `name` is a bundle-declared hook point (`<bundle-id>:<point>`).
#[must_use]
pub fn is_declared_hook_point(name: &str) -> bool {
    name.split_once(':')
        .is_some_and(|(bundle, point)| !bundle.is_empty() && !point.is_empty())
}

/// Input passed to `tool.execute.before` native hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeToolBeforeInput {
    /// Session executing the tool.
    pub session: SessionId,
    /// Assistant message id.
    pub message: MessageId,
    /// Tool call id.
    pub call: ToolCallId,
    /// Canonical tool name.
    pub tool: String,
    /// Tool input arguments JSON.
    pub input: Value,
}

/// Outcome returned by `tool.execute.before` native hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NativeToolBeforeOutcome {
    /// Continue execution with (possibly rewritten) input.
    Continue {
        /// Tool arguments JSON.
        input: Value,
    },
    /// Veto tool execution with a diagnostic reason.
    Veto {
        /// Human/model-visible veto reason.
        reason: String,
    },
}

/// Native tool execution result passed to `tool.execute.after` hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeToolResult {
    /// Successful tool JSON output and elapsed duration.
    Ok {
        /// Tool JSON output.
        output: Value,
        /// Execution duration in milliseconds.
        time_ms: u64,
    },
    /// Failed tool execution with error text.
    Err {
        /// Display error text.
        message: String,
    },
}

/// Input passed to `tool.execute.after` native hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeToolAfterInput {
    /// Session that ran the tool.
    pub session: SessionId,
    /// Assistant message id.
    pub message: MessageId,
    /// Tool call id.
    pub call: ToolCallId,
    /// Canonical tool name.
    pub tool: String,
    /// Executed tool input arguments.
    pub input: Value,
    /// Tool outcome prior to hook processing.
    pub result: NativeToolResult,
}

/// Outcome returned by `tool.execute.after` native hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NativeToolAfterOutcome {
    /// Continue with (possibly rewritten) tool result.
    Continue {
        /// Updated outcome.
        result: NativeToolResult,
    },
}

/// Input passed to `tool.visibility` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVisibilityInput {
    /// Session preparing a turn.
    pub session: SessionId,
    /// Bound agent name for the turn, if known.
    pub agent: Option<AgentName>,
    /// Candidate tool names before visibility filtering.
    pub candidate_tools: Vec<String>,
    /// Whether an interactive user-question channel is currently connected.
    #[serde(default)]
    pub interactive: bool,
    /// Number of parent links between the session and its tree root.
    #[serde(default)]
    pub depth: u32,
}

/// Outcome returned by `tool.visibility` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolVisibilityOutcome {
    /// Leave the candidate tool list unchanged.
    Preserve,
    /// Restrict visibility by excluding these tool names.
    Hide {
        /// Tool names to hide from the model for this turn.
        tools: Vec<String>,
    },
    /// Restrict visibility strictly to the listed tool names (intersection).
    AllowOnly {
        /// Permitted tool names for this turn.
        tools: Vec<String>,
    },
}

/// Trigger classification for `compaction.before`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompactionTrigger {
    /// Context exceeded threshold; compaction is mandatory.
    Overflow,
    /// Proactive pre-threshold compaction.
    Proactive,
}

/// Input passed to `compaction.before` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCompactionBeforeInput {
    /// Session whose context is about to be compacted.
    pub session: SessionId,
    /// Trigger reason.
    pub trigger: NativeCompactionTrigger,
    /// Estimated token occupancy prior to compaction.
    pub messages_token_estimate: usize,
}

/// Decision returned by `compaction.before` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum NativeCompactionDecision {
    /// Proceed with standard compaction.
    Proceed,
    /// Request skipping proactive compaction.
    Skip {
        /// Reason shown in warning logs.
        reason: String,
    },
    /// Replace summarizer instructions for this compaction pass.
    Replace {
        /// Replacement summarizer prompt instructions.
        instructions: String,
    },
}

/// Input passed to `compaction.after` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCompactionAfterInput {
    /// Session whose context was compacted.
    pub session: SessionId,
    /// Estimated token size of the committed summary.
    pub summary_tokens: usize,
}

/// Input passed to `session.start` and `session.end` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSessionLifecycleInput {
    /// Affected session identifier.
    pub session: SessionId,
}

/// Input passed to `agent.spawn` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAgentSpawnInput {
    /// Parent session identifier.
    pub parent: SessionId,
    /// Newly registered child session identifier.
    pub child: SessionId,
}

/// Input passed to `command.execute.before` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCommandBeforeInput {
    /// Session running the command.
    pub session: SessionId,
    /// Command name.
    pub command: String,
    /// Argument string.
    pub arguments: String,
    /// Full command text.
    pub text: String,
}

/// Input passed to `message.user.before` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeMessageUserBeforeInput {
    /// Target session id.
    pub session: SessionId,
    /// Raw user text.
    pub text: String,
}

/// Input passed to `text.complete` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTextCompleteInput {
    /// Session owning the message.
    pub session: SessionId,
    /// Message id.
    pub message: MessageId,
    /// Text part id.
    pub part: PartId,
    /// Completed assistant text.
    pub text: String,
}

/// Input passed to `chat.params` native hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeChatParamsInput {
    /// Session for the completion.
    pub session: SessionId,
    /// Root session of the spawn tree.
    pub root_session: SessionId,
    /// Bound agent id, if known.
    pub agent: Option<AgentName>,
    /// Assistant message id.
    pub message: MessageId,
    /// Serialized completion request parameters.
    pub params: Value,
}

/// Input passed to `model.fallback` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeModelFallbackInput {
    /// Session making the completion.
    pub session: SessionId,
    /// Root session of the spawn tree.
    pub root_session: SessionId,
    /// Bound agent id, if known.
    pub agent: Option<AgentName>,
    /// Assistant message id.
    pub message: MessageId,
    /// Model that just failed.
    pub model: ModelRef,
    /// Error classification tag (`retryable`, `unknown_model`, `auth`, `invalid_request`, `other`).
    pub error_class: String,
    /// Diagnostic failure text.
    pub error_message: String,
    /// 1-based failed attempt count.
    pub attempt: u32,
    /// Models already tried in this round.
    pub tried: Vec<ModelRef>,
}

/// Input passed to `permission.approve` native hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePermissionApproveInput {
    /// Session whose tool call triggered the ask.
    pub session: SessionId,
    /// Root session owning the permission mode.
    pub root_session: SessionId,
    /// Bound agent id, if known.
    pub agent: Option<AgentName>,
    /// Bundle-local permission mode id.
    pub mode: String,
    /// Permission action requested.
    pub action: Action,
    /// Target resource.
    pub resource: Resource,
}

/// The session, tree root, and agent binding of a turn being admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTurnAdmitInput {
    /// Session running the turn.
    pub session: SessionId,
    /// The top ancestor, or `session` for a root turn.
    pub root: SessionId,
    /// The session's bound agent, when it has a recorded binding.
    pub agent: Option<AgentName>,
    /// The fence of a bundle-driven turn; `None` for a client turn.
    pub actor_claim: Option<ActorClaim>,
}

/// Input passed to `model.stream.permit` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeStreamPermitInput {
    /// Session opening the stream.
    pub session: SessionId,
    /// Root of the session's tree.
    pub root: SessionId,
    /// Number of parent links between `session` and `root`.
    pub depth: u32,
}

/// Guard a `model.stream.permit` hook returns. The host drops it when the
/// round's stream is collected or before it waits to retry the round.
pub struct NativeStreamPermit(pub Box<dyn Send + Sync>);

/// Input passed to `turn.stop` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTurnStopInput {
    /// Session the user stopped.
    pub session: SessionId,
    /// Cause the host records on the cancelled turns.
    pub cause: FinishCause,
}

/// What `session.close.before` is about to do to a session tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseKind {
    /// Archive the session and its descendants.
    Archive,
    /// Delete the session log.
    Delete,
}

/// Input passed to `session.close.before` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSessionCloseInput {
    /// Session being closed (the root of the closed tree).
    pub session: SessionId,
    /// Archive or delete.
    pub kind: SessionCloseKind,
}

/// Where a headless run is when `run.headless` is dispatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum HeadlessRunPhase {
    /// Before the first turn.
    Start,
    /// After a turn ended with `finish`.
    TurnEnded {
        /// The ended turn's finish reason.
        finish: FinishReason,
    },
    /// Before the run exits.
    End,
}

/// Input passed to `run.headless` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeHeadlessRunInput {
    /// Session the headless run drives.
    pub session: SessionId,
    /// Point of the run.
    pub phase: HeadlessRunPhase,
}

/// Input passed to `session.restore` native hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSessionRestoreInput {
    /// Session whose interrupted turn is being restored.
    pub session: SessionId,
}

/// One hook's decision about a turn.
pub enum NativeTurnAdmitOutcome {
    /// Continue to the next admission hook.
    Admit,
    /// Wait for a readiness signal before continuing the chain.
    Defer {
        /// A closed sender refuses admission. Turn cancellation interrupts the wait.
        until: tokio::sync::oneshot::Receiver<()>,
    },
    /// Refuse the turn and discard any accumulated steering.
    Refuse {
        /// Diagnostic presented to the caller.
        reason: String,
    },
    /// Add a system message if the chain admits the turn.
    Steer {
        /// Text of the system message.
        message: String,
    },
}

/// Hook implementation registered by a native bundle.
#[async_trait]
pub trait NativeHook: Send + Sync {
    /// Unique hook identifier within the bundle.
    fn id(&self) -> &str;

    /// Hook points this implementation handles.
    fn hook_points(&self) -> &[HookPoint];

    /// Intercept or veto tool execution before running (`tool.execute.before`).
    async fn tool_execute_before(
        &self,
        input: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, HostError> {
        Ok(NativeToolBeforeOutcome::Continue { input: input.input })
    }

    /// Rewrite tool results or errors after running (`tool.execute.after`).
    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        Ok(NativeToolAfterOutcome::Continue {
            result: input.result,
        })
    }

    /// Filter visible tools for a turn (`tool.visibility`).
    async fn tool_visibility(
        &self,
        _input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        Ok(ToolVisibilityOutcome::Preserve)
    }

    /// Consulted before context compaction (`compaction.before`).
    async fn compaction_before(
        &self,
        _input: NativeCompactionBeforeInput,
    ) -> Result<NativeCompactionDecision, HostError> {
        Ok(NativeCompactionDecision::Proceed)
    }

    /// Notified after context compaction (`compaction.after`).
    async fn compaction_after(&self, _input: NativeCompactionAfterInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Notified on session start (`session.start`).
    async fn session_start(&self, _input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Notified on session end (`session.end`).
    async fn session_end(&self, _input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Notified on child subagent spawn (`agent.spawn`).
    async fn agent_spawn(&self, _input: NativeAgentSpawnInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Rewrite shell command text before execution (`command.execute.before`).
    async fn command_execute_before(
        &self,
        input: NativeCommandBeforeInput,
    ) -> Result<String, HostError> {
        Ok(input.text)
    }

    /// Rewrite user text before message admission (`message.user.before`).
    async fn message_user_before(
        &self,
        input: NativeMessageUserBeforeInput,
    ) -> Result<String, HostError> {
        Ok(input.text)
    }

    /// Inspect turn admission before the provider starts.
    async fn turn_admit(
        &self,
        _input: NativeTurnAdmitInput,
    ) -> Result<NativeTurnAdmitOutcome, HostError> {
        Ok(NativeTurnAdmitOutcome::Admit)
    }

    /// Rewrite completed assistant text (`text.complete`).
    async fn text_complete(&self, input: NativeTextCompleteInput) -> Result<String, HostError> {
        Ok(input.text)
    }

    /// Adjust completion request JSON parameters (`chat.params`).
    async fn chat_params(&self, input: NativeChatParamsInput) -> Result<Value, HostError> {
        Ok(input.params)
    }

    /// Propose a fallback model after a pre-stream failure (`model.fallback`).
    async fn model_fallback(
        &self,
        _input: NativeModelFallbackInput,
    ) -> Result<Option<ModelRef>, HostError> {
        Ok(None)
    }

    /// Intercept a permission ask (`permission.ask`).
    async fn permission_ask(
        &self,
        _session: Option<SessionId>,
        _action: Action,
        _resource: &Resource,
    ) -> Result<Option<Decision>, HostError> {
        Ok(None)
    }

    /// Approve under a bundle-declared permission mode (`permission.approve`).
    async fn permission_approve(
        &self,
        _input: NativePermissionApproveInput,
    ) -> Result<Option<Decision>, HostError> {
        Ok(None)
    }

    /// Bundle-declared hook points (`<bundle-id>:<point>`) this hook implements.
    fn declared_points(&self) -> &[String] {
        &[]
    }

    /// Answer a bundle-declared hook point listed in [`Self::declared_points`].
    ///
    /// `Ok(None)` defers to the next implementer. The declaring bundle owns
    /// the payload shapes.
    async fn invoke_declared(&self, point: &str, input: Value) -> Result<Option<Value>, HostError> {
        let _ = (point, input);
        Ok(None)
    }

    /// Whether the hook's process can still serve calls; an unhealthy hook
    /// cancels the turn that checks it.
    fn is_healthy(&self) -> bool {
        true
    }

    /// Stable identity of this hook's `permission.ask` decisions, used to pin
    /// saved permission grants. `None` means the hook does not pin.
    fn permission_identity(&self) -> Option<[u8; 32]> {
        None
    }

    /// Observe a live envelope published on the event bus (`event`).
    ///
    /// Called synchronously in publish order; implementors must not block.
    fn on_event(&self, envelope: &Envelope) {
        let _ = envelope;
    }

    /// Wait for capacity to open a model stream (`model.stream.permit`).
    /// `Ok(None)` grants it without a guard.
    async fn model_stream_permit(
        &self,
        _input: NativeStreamPermitInput,
    ) -> Result<Option<NativeStreamPermit>, HostError> {
        Ok(None)
    }

    /// Notified before the host cancels a user-stopped turn (`turn.stop`).
    async fn turn_stop(&self, _input: NativeTurnStopInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Consulted before a session tree is archived or deleted
    /// (`session.close.before`); an error refuses the close.
    async fn session_close(&self, _input: NativeSessionCloseInput) -> Result<(), HostError> {
        Ok(())
    }

    /// Report a line of pending work for a headless run (`run.headless`).
    async fn headless_run(
        &self,
        _input: NativeHeadlessRunInput,
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    /// Text appended to a session's restore prompt (`session.restore`).
    async fn session_restore(
        &self,
        _input: NativeSessionRestoreInput,
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    /// Notified once restore prompts have been admitted (`runtime.restored`).
    async fn runtime_restored(&self) -> Result<(), HostError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NativeBundleApiHandler
// ---------------------------------------------------------------------------

/// Endpoint metadata declared by a [`NativeBundleApiHandler`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeApiEndpoint {
    /// Endpoint identifier matching the bundle manifest `apis:` entry.
    pub id: String,
    /// HTTP method.
    pub method: ApiMethod,
    /// Mount scope (`session` or `global`).
    pub scope: ApiScope,
    /// Path template relative to the bundle mount (e.g. `"/runs/{id}"`).
    pub path: String,
    /// Optional human-readable description.
    pub description: String,
}

/// Routed bundle API request delivered to a [`NativeBundleApiHandler`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeBundleApiRequest {
    /// Matched endpoint id.
    pub api: String,
    /// HTTP method.
    pub method: ApiMethod,
    /// Mount scope (`session` or `global`).
    pub scope: ApiScope,
    /// Concrete request path below the bundle mount.
    pub path: String,
    /// Path template parameters extracted from `path`.
    pub path_params: BTreeMap<String, String>,
    /// Query string parameters.
    pub query: BTreeMap<String, String>,
    /// JSON request body (`Value::Null` when absent).
    pub body: Value,
    /// Bound session id for session-scoped endpoints.
    pub session: Option<SessionId>,
}

/// Response returned by a [`NativeBundleApiHandler`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeBundleApiReply {
    /// HTTP status code (`200..=599`).
    pub status: u16,
    /// JSON response body (`Value::Null` for empty responses).
    pub body: Value,
}

/// Request handler for native bundle HTTP/gRPC `BundleApi` routes.
#[async_trait]
pub trait NativeBundleApiHandler: Send + Sync {
    /// Handler identifier within the bundle.
    fn id(&self) -> &str;

    /// Endpoints served by this handler.
    fn endpoints(&self) -> &[NativeApiEndpoint];

    /// Serve one routed request.
    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError>;
}

// ---------------------------------------------------------------------------
// ProjectionReducer
// ---------------------------------------------------------------------------

/// Metadata and payload of an `Event::Extension` passed to [`ProjectionReducer::apply`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEventContext {
    /// Session owning the event log.
    pub session: SessionId,
    /// Monotonic durable sequence number.
    pub seq: EventSeq,
    /// Event timestamp in Unix epoch milliseconds.
    pub ts_millis: i64,
    /// Owning bundle identity.
    pub bundle: String,
    /// Bundle-defined extension event kind string.
    pub kind: String,
    /// Extension event JSON payload.
    pub payload: Value,
}

/// A tool output an extension event replaces in later requests: the host
/// records `output` as the tool part's `PartProjection::Tool::eviction_notice`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutputOverride {
    /// Tool part whose output is replaced.
    pub part: PartId,
    /// Text sent in place of the output.
    pub output: String,
}

/// Folds bundle-owned `Event::Extension` records into `Projection` extension JSON state.
pub trait ProjectionReducer: Send + Sync {
    /// Owning bundle identity (key under `Projection::extensions`).
    fn bundle_id(&self) -> &str;

    /// Initial JSON state before any extension events are applied.
    fn initial_state(&self) -> Value {
        Value::Null
    }

    /// Fold one extension event into `state` deterministically and idempotently.
    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError>;

    /// Tool outputs `event` replaces in the transcript, applied by the host
    /// after [`Self::apply`] succeeds. Deterministic, like `apply`.
    fn tool_output_overrides(&self, event: &ExtensionEventContext) -> Vec<ToolOutputOverride> {
        let _ = event;
        Vec::new()
    }

    /// Other bundles whose events the host also passes to [`Self::apply`]
    /// (with `event.bundle` naming the owner), folding into this reducer's
    /// state only.
    fn observed_bundles(&self) -> &[&str] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// UriSchemaHandler
// ---------------------------------------------------------------------------

/// Context supplied when resolving or writing a custom URI handle.
pub struct UriResolveCtx<'a> {
    /// Session id requesting the handle, if known.
    pub session: Option<SessionId>,
    /// Optional actor claim fencing resident/subagent side effects.
    pub actor_claim: Option<ActorClaim>,
    /// Session working directory.
    pub workdir: &'a Path,
    /// Optional host runtime reference for backing lookups.
    pub runtime: Option<&'a dyn HostRuntime>,
}

/// Resolved content returned by [`UriSchemaHandler::read`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UriContent {
    /// Resolved UTF-8 text body.
    pub body: String,
    /// IANA media type (e.g. `"text/plain"`, `"application/json"`, `"text/markdown"`).
    pub media_type: String,
    /// Optional structured metadata.
    pub metadata: Option<Value>,
}

/// Handler for a custom URI scheme (such as `channel://`) integrated with `HandleRouter`.
#[async_trait]
pub trait UriSchemaHandler: Send + Sync {
    /// URI scheme token before `://` (e.g. `"channel"`).
    fn scheme(&self) -> &str;

    /// Whether writes through [`UriSchemaHandler::write`] are permitted.
    fn writable(&self) -> bool {
        false
    }

    /// Resolve a full URI reference (e.g. `channel://main?last=20`).
    async fn read(&self, ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError>;

    /// Write content to a custom URI reference when [`UriSchemaHandler::writable`] is `true`.
    async fn write(
        &self,
        _ctx: UriResolveCtx<'_>,
        uri: &str,
        _content: &str,
    ) -> Result<Value, HostError> {
        Err(HostError::PermissionDenied(format!(
            "{}:// is read-only ({uri})",
            self.scheme()
        )))
    }
}

/// Adapts an ABI v2 [`UriSchemaHandler`] into `hya_tool::handle::DynamicUriHandler`
/// for registration on [`hya_tool::handle::HandleRouter`].
pub struct UriSchemaHandleAdapter {
    handler: Arc<dyn UriSchemaHandler>,
    runtime: Option<Arc<dyn HostRuntime>>,
}

impl UriSchemaHandleAdapter {
    /// Wrap `handler` with an optional [`HostRuntime`] injected on every call.
    #[must_use]
    pub fn new(handler: Arc<dyn UriSchemaHandler>, runtime: Option<Arc<dyn HostRuntime>>) -> Self {
        Self { handler, runtime }
    }
}

/// Adapt an ABI v2 [`UriSchemaHandler`] into a [`hya_tool::handle::DynamicUriHandler`]
/// ready for [`hya_tool::handle::HandleRouter::with_uri_handler`] or
/// [`hya_tool::ToolCtx::with_uri_handler`].
#[must_use]
pub fn adapt_uri_schema_handler(
    handler: Arc<dyn UriSchemaHandler>,
    runtime: Option<Arc<dyn HostRuntime>>,
) -> Arc<dyn hya_tool::handle::DynamicUriHandler> {
    Arc::new(UriSchemaHandleAdapter::new(handler, runtime))
}

fn map_host_error_to_handle_error(error: HostError) -> hya_tool::handle::HandleError {
    match error {
        HostError::NotFound(msg) => hya_tool::handle::HandleError::NotFound(msg),
        HostError::InvalidInput(msg) => hya_tool::handle::HandleError::MalformedPath(msg),
        HostError::PermissionDenied(msg) => hya_tool::handle::HandleError::PermissionDenied(msg),
        other => hya_tool::handle::HandleError::Handler(other.to_string()),
    }
}

#[async_trait]
impl hya_tool::handle::DynamicUriHandler for UriSchemaHandleAdapter {
    fn scheme(&self) -> &str {
        self.handler.scheme()
    }

    fn writable(&self) -> bool {
        self.handler.writable()
    }

    async fn read(
        &self,
        ctx: hya_tool::handle::DynamicUriCtx<'_>,
        uri: &str,
    ) -> Result<hya_tool::handle::DynamicUriContent, hya_tool::handle::HandleError> {
        let content = self
            .handler
            .read(
                UriResolveCtx {
                    session: ctx.session,
                    actor_claim: ctx.actor_claim,
                    workdir: ctx.workdir,
                    runtime: self.runtime.as_deref(),
                },
                uri,
            )
            .await
            .map_err(map_host_error_to_handle_error)?;
        Ok(hya_tool::handle::DynamicUriContent {
            body: content.body,
            media_type: content.media_type,
            metadata: content.metadata,
        })
    }

    async fn write(
        &self,
        ctx: hya_tool::handle::DynamicUriCtx<'_>,
        uri: &str,
        content: &str,
    ) -> Result<Value, hya_tool::handle::HandleError> {
        self.handler
            .write(
                UriResolveCtx {
                    session: ctx.session,
                    actor_claim: ctx.actor_claim,
                    workdir: ctx.workdir,
                    runtime: self.runtime.as_deref(),
                },
                uri,
                content,
            )
            .await
            .map_err(map_host_error_to_handle_error)
    }
}

// ---------------------------------------------------------------------------
// NativeDriver
// ---------------------------------------------------------------------------

/// Input of one [`NativeDriver`] run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverRequest {
    /// Session the run drives.
    pub session: SessionId,
    /// Agent bound to the session.
    pub agent: Option<AgentName>,
    /// The session's worker model.
    pub model: Option<ModelRef>,
    /// Working directory of the run.
    pub workdir: std::path::PathBuf,
    /// Driver arguments; for a CLI command, its parsed arguments by id.
    pub args: Value,
}

/// What a driver run leaves for its caller to print, and its exit code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverReport {
    /// Text for standard output.
    pub stdout: String,
    /// Text for standard error.
    pub stderr: String,
    /// Process exit code (`0` on success).
    pub exit_code: i32,
}

/// Terminal outcome of an iterating driver run (goal or loop mode).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DriverRunOutcome {
    /// Target or goal condition was satisfied.
    Achieved {
        /// Iterations completed.
        iterations: u32,
        /// Reason reported by evaluator/verifier.
        reason: String,
    },
    /// Hit a safety cap before satisfaction.
    Capped {
        /// Iterations completed.
        iterations: u32,
        /// Name of the triggered cap (`max_iterations`, `max_wall_clock`, etc.).
        which: String,
    },
    /// Hit a safety cap and finished one wrap-up pass.
    BudgetLimited {
        /// Iterations completed before wrap-up.
        iterations: u32,
        /// Name of the triggered cap.
        which: String,
    },
    /// Progress stalled before budget exhaustion.
    Stalled {
        /// Iterations completed.
        iterations: u32,
    },
    /// Deterministic condition could not be evaluated.
    InvalidCondition {
        /// Iterations completed.
        iterations: u32,
        /// Condition failure diagnostic.
        reason: String,
    },
    /// Cooperative cancellation was triggered.
    Cancelled,
}

/// A run that owns a session until it returns: an iteration strategy such as
/// goal mode or loop mode. The host knows only that a driver of this name
/// exists (`<bundle-id>:<name>`).
#[async_trait]
pub trait NativeDriver: Send + Sync {
    /// Driver name, local to its bundle.
    fn name(&self) -> &str;

    /// Run to completion.
    async fn run(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: DriverRequest,
        cancel: CancellationToken,
    ) -> Result<DriverReport, HostError>;
}
