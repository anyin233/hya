//! Native bundle extension traits registered through ABI v2.
//!
//! Alongside [`hya_tool::Tool`] and [`hya_context::rewrite::Rewriter`], native
//! bundles may register lifecycle services, runtime hooks, bundle HTTP/gRPC API
//! handlers, event projection reducers, custom URI scheme handlers, and goal or
//! loop drivers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_proto::{
    AgentName, Envelope, EventSeq, MessageId, ModelRef, PartId, SessionId, ToolCallId,
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
    /// Evaluate a goal completion condition (`goal.evaluate`).
    GoalEvaluate,
    /// Verify loop target progress (`loop.verifier`).
    LoopVerify,
    /// Plan next loop directive (`loop.planner`).
    LoopPlan,
    /// Decide whether a loop should stop (`loop.should_stop`).
    LoopShouldStop,
    /// Observe live envelopes published on the event bus (`event`).
    EventObserve,
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

/// Verdict returned by a `goal.evaluate` native hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeGoalEvaluateVerdict {
    /// Structured verdict.
    Verdict {
        /// Whether the goal condition is satisfied.
        met: bool,
        /// Explanation for logs and directives.
        reason: String,
    },
    /// Reply was unparseable; driver counts it against iteration budget.
    Malformed,
}

/// Structured verdict returned by a `loop.verifier` native hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeVerifierVerdict {
    /// Score in `0..=100`.
    pub score: u8,
    /// Whether the verifier considers the target satisfied.
    pub satisfied: bool,
    /// Evidence quality band (`missing`, `claim_only`, `supported`, `verified`).
    pub evidence_quality: String,
    /// Remaining critical gaps.
    pub critical_gaps: Vec<String>,
    /// Summary of this iteration's progress.
    pub iteration_summary: String,
    /// Free-form reason string.
    pub reason: String,
}

/// Input passed to a `loop.planner` native hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeLoopPlanInput {
    /// Loop target description.
    pub target: String,
    /// Historical directive/summary strings.
    pub history: Vec<String>,
    /// Last verifier verdict.
    pub last: NativeVerifierVerdict,
    /// Persistent planner notes.
    pub planner_notes: String,
}

/// Planned next directive returned by a `loop.planner` native hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePlannerOutput {
    /// Next directive for the worker turn.
    pub directive: String,
    /// Continuity brief for the worker.
    pub continuity_brief: String,
    /// Updated planner notes.
    pub planner_notes: String,
    /// Whether strategy changed.
    pub strategy_change: bool,
    /// Explanation of strategy change.
    pub change_note: String,
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

    /// Evaluate a goal condition (`goal.evaluate`).
    async fn goal_evaluate(
        &self,
        _condition: &str,
        _transcript: &str,
    ) -> Result<Option<NativeGoalEvaluateVerdict>, HostError> {
        Ok(None)
    }

    /// Grade a loop target (`loop.verifier`).
    async fn loop_verify(
        &self,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<NativeVerifierVerdict>, HostError> {
        Ok(None)
    }

    /// Plan the next loop directive (`loop.planner`).
    async fn loop_plan(
        &self,
        _input: NativeLoopPlanInput,
    ) -> Result<Option<NativePlannerOutput>, HostError> {
        Ok(None)
    }

    /// Decide whether a loop should stop (`loop.should_stop`).
    async fn loop_should_stop(
        &self,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    /// Observe a live event envelope (`event`).
    async fn on_event(&self, _envelope: &Envelope) -> Result<(), HostError> {
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
}

// ---------------------------------------------------------------------------
// UriSchemaHandler
// ---------------------------------------------------------------------------

/// Context supplied when resolving or writing a custom URI handle.
pub struct UriResolveCtx<'a> {
    /// Session id requesting the handle, if known.
    pub session: Option<SessionId>,
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
// GoalDriver & LoopDriver
// ---------------------------------------------------------------------------

/// Request parameters passed to a [`GoalDriver`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalRunRequest {
    /// Target session executing the goal run.
    pub session: SessionId,
    /// Goal completion condition text.
    pub goal: String,
    /// Maximum iterations allowed before capping.
    pub max_iterations: u32,
    /// Maximum wall-clock duration in seconds.
    pub max_wall_clock_secs: u64,
    /// Optional agent id for worker turns.
    pub agent: Option<AgentName>,
    /// Optional model override.
    pub model: Option<ModelRef>,
    /// Bundle-specific driver configuration JSON.
    pub config: Value,
}

/// Request parameters passed to a [`LoopDriver`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRunRequest {
    /// Target session executing the loop run.
    pub session: SessionId,
    /// Loop target objective.
    pub target: String,
    /// Maximum iteration budget.
    pub budget: u32,
    /// Maximum wall-clock duration in seconds.
    pub max_wall_clock_secs: u64,
    /// Minimum verifier score treated as satisfied (`0..=100`).
    pub satisfaction_threshold: u8,
    /// Consecutive no-progress iterations before stopping as stalled.
    pub max_no_progress: u32,
    /// Optional deterministic predicate configuration/command JSON.
    pub predicate: Option<Value>,
    /// Optional agent id for worker turns.
    pub agent: Option<AgentName>,
    /// Optional model override.
    pub model: Option<ModelRef>,
    /// Bundle-specific driver configuration JSON.
    pub config: Value,
}

/// Terminal outcome of a [`GoalDriver`] or [`LoopDriver`] execution.
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

/// Native driver executing goal-mode iterative runs (`hya/goal`).
#[async_trait]
pub trait GoalDriver: Send + Sync {
    /// Stable driver identifier (e.g. `"goal"`).
    fn id(&self) -> &str;

    /// Run a goal loop until achieved, capped, stalled, or cancelled.
    async fn run_goal(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError>;
}

/// Native driver executing verifier+planner loop runs (`hya/loop`).
#[async_trait]
pub trait LoopDriver: Send + Sync {
    /// Stable driver identifier (e.g. `"loop"`).
    fn id(&self) -> &str;

    /// Run a verified loop until satisfied, capped, stalled, or cancelled.
    async fn run_loop(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: LoopRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError>;
}
