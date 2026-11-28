//! Host runtime capability traits injected into native bundles via ABI v2.
//!
//! Native bundle dynamic libraries must never depend on `hya-store`, `sqlx`,
//! `hya-engine`, `hya-server`, `axum`, or `tonic`. Instead, the host injects an
//! `Arc<dyn HostRuntime>` providing typed access to session logs, generic bundle
//! KV/claim storage, turn execution, model calls, the event bus, catalogs, and
//! registered extensions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, Bundle, BundleIdentity, PreparedAgent};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef,
    ProjectId, Projection, SessionId, TokenUsage, ToolCallId, ToolName, ToolSchema, UsagePurpose,
};
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::error::HostError;
use crate::extensions::{
    GoalDriver, LoopDriver, NativeGoalEvaluateVerdict, NativeLoopPlanInput, NativePlannerOutput,
    NativeVerifierVerdict,
};

// ---------------------------------------------------------------------------
// HostSessionStore
// ---------------------------------------------------------------------------

/// Lightweight metadata summary for a persisted session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummaryMeta {
    /// Session identifier.
    pub session: SessionId,
    /// Parent session id when spawned as a subagent/child.
    pub parent: Option<SessionId>,
    /// Bound agent id, if known.
    pub agent: Option<AgentName>,
    /// Bound model reference, if known.
    pub model: Option<ModelRef>,
    /// Session working directory.
    pub workdir: Option<String>,
    /// Display title, if set.
    pub title: Option<String>,
    /// Owning project id, if any.
    pub project: Option<ProjectId>,
    /// Whether the session is archived.
    pub archived: bool,
    /// Arbitrary session metadata JSON.
    pub metadata: Value,
    /// Number of durable events currently recorded for this session.
    pub event_count: u64,
}

/// Filter parameters for listing sessions through [`HostSessionStore`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListFilter {
    /// Restrict to direct children of `parent`.
    pub parent: Option<SessionId>,
    /// Restrict to sessions belonging to `project`.
    pub project: Option<ProjectId>,
    /// Include archived sessions when `true`.
    pub include_archived: bool,
    /// Optional maximum number of sessions to return.
    pub limit: Option<usize>,
}

/// Session event log and projection access provided by the host.
#[async_trait]
pub trait HostSessionStore: Send + Sync {
    /// Check whether a session exists in the store.
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError>;

    /// Read durable envelopes for `session` after `after_seq`, optionally capped at `limit`.
    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError>;

    /// Append one atomic batch of domain events to `session`.
    async fn append_events(
        &self,
        session: SessionId,
        claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError>;

    /// Append a bundle-owned extension event (`Event::Extension`) to `session`.
    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError>;

    /// Read the folded projection for `session`.
    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError>;

    /// Read folded extension state for `(session, bundle)`, if any exists.
    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError>;

    /// Read summary metadata for `session`.
    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError>;

    /// List sessions matching `filter`.
    async fn list_sessions(
        &self,
        filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError>;
}

// ---------------------------------------------------------------------------
// HostBundleStorage
// ---------------------------------------------------------------------------

/// One key-value entry stored in the host's `bundle_state` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleStateEntry {
    /// Owning bundle identity (e.g. `hya/multi-agent`).
    pub bundle: String,
    /// Logical scope key (e.g. `global`, `session:<id>`, `project:<id>`).
    pub scope: String,
    /// Entry key within `(bundle, scope)`.
    pub key: String,
    /// JSON value payload.
    pub value: Value,
    /// Monotonic optimistic-concurrency version counter.
    pub version: u64,
    /// Last update timestamp in Unix epoch milliseconds.
    pub updated_at_millis: i64,
}

/// Request to acquire or take over a fenced lease in `bundle_claim`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleClaimRequest {
    /// Owning bundle identity.
    pub bundle: String,
    /// Unique resource key within `bundle`.
    pub resource_key: String,
    /// Logical owner identifier acquiring the claim.
    pub owner_id: String,
    /// Requested lease duration in milliseconds.
    pub ttl_millis: u64,
    /// Optional metadata stored with the lease.
    pub metadata: Value,
}

/// Active lease returned by `bundle_claim` acquisition or renewal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleClaimLease {
    /// Owning bundle identity.
    pub bundle: String,
    /// Claimed resource key.
    pub resource_key: String,
    /// Current owner identifier.
    pub owner_id: String,
    /// Monotonic fencing epoch incremented on each acquisition.
    pub epoch: u64,
    /// Expiration timestamp in Unix epoch milliseconds.
    pub expires_at_millis: i64,
    /// Metadata attached to the lease.
    pub metadata: Value,
}

/// Generic bundle KV (`bundle_state`) and fenced lease (`bundle_claim`) storage.
#[async_trait]
pub trait HostBundleStorage: Send + Sync {
    /// Read one `bundle_state` entry.
    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError>;

    /// Write one `bundle_state` entry, optionally enforcing `expected_version` CAS.
    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError>;

    /// Delete one `bundle_state` entry, returning `true` if a row was removed.
    async fn delete_state(&self, bundle: &str, scope: &str, key: &str) -> Result<bool, HostError>;

    /// List all `bundle_state` entries under `(bundle, scope)` matching an optional key prefix.
    async fn list_state(
        &self,
        bundle: &str,
        scope: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError>;

    /// Acquire a fenced lease on `(bundle, resource_key)`.
    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError>;

    /// Extend an existing lease; returns [`HostError::StaleClaim`] if superseded.
    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError>;

    /// Release an active lease; returns [`HostError::StaleClaim`] if superseded.
    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError>;

    /// Inspect the current claim on `(bundle, resource_key)`, if any.
    async fn read_claim(
        &self,
        bundle: &str,
        resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError>;
}

// ---------------------------------------------------------------------------
// HostTurnRunner
// ---------------------------------------------------------------------------

/// Request to execute a single turn on an existing session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStartRequest {
    /// Target session id.
    pub session: SessionId,
    /// User/directive prompt text for this turn.
    pub prompt: String,
    /// Optional agent override for this turn.
    pub agent: Option<AgentName>,
    /// Optional model override for this turn.
    pub model: Option<ModelRef>,
    /// Optional system prompt addendum or override.
    pub system_override: Option<String>,
    /// Optional turn metadata payload.
    pub metadata: Option<Value>,
    /// Optional actor claim fencing this turn.
    pub actor_claim: Option<ActorClaim>,
}

/// Request to spawn a new session (and optionally immediately run a turn).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSpawnRequest {
    /// Optional parent session id.
    pub parent: Option<SessionId>,
    /// Agent definition id to bind.
    pub agent: AgentName,
    /// Optional model override.
    pub model: Option<ModelRef>,
    /// Optional working directory override.
    pub workdir: Option<PathBuf>,
    /// Optional display title for the created session.
    pub title: Option<String>,
    /// Initial prompt text when running immediately.
    pub prompt: String,
    /// Optional initial metadata JSON.
    pub metadata: Option<Value>,
}

/// Terminal outcome of a driven turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutcome {
    /// Session that ran the turn.
    pub session: SessionId,
    /// Produced assistant message id, if any.
    pub message_id: Option<MessageId>,
    /// Terminal finish reason, if completed.
    pub finish_reason: Option<FinishReason>,
    /// Final visible assistant text produced by the turn.
    pub output_text: String,
    /// Error message when the turn failed.
    pub error: Option<String>,
}

/// Live execution status of a session's turn loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum TurnStatus {
    /// Session is idle with no active turn.
    Idle,
    /// A turn is currently running.
    Running,
    /// Last turn completed cleanly.
    Completed,
    /// Last turn was cancelled.
    Cancelled,
    /// Last turn failed with a diagnostic.
    Failed(String),
}

/// Session creation and turn-driving capability without linking `hya-engine`.
#[async_trait]
pub trait HostTurnRunner: Send + Sync {
    /// Create a session according to `request` without starting a turn.
    async fn spawn_session(&self, request: TurnSpawnRequest) -> Result<SessionId, HostError>;

    /// Run a turn on an existing session and await its [`TurnOutcome`].
    async fn run_turn(
        &self,
        request: TurnStartRequest,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError>;

    /// Spawn a new session and execute its initial turn.
    async fn spawn_session_and_run(
        &self,
        request: TurnSpawnRequest,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError>;

    /// Deliver an in-flight steer message to a running session.
    async fn steer_turn(&self, session: SessionId, message: String) -> Result<(), HostError>;

    /// Request cancellation of an active turn; returns `true` if a live turn was signalled.
    async fn cancel_turn(
        &self,
        session: SessionId,
        reason: Option<String>,
    ) -> Result<bool, HostError>;

    /// Query the current turn status of `session`.
    async fn turn_status(&self, session: SessionId) -> Result<TurnStatus, HostError>;
}

// ---------------------------------------------------------------------------
// HostModelCaller
// ---------------------------------------------------------------------------

/// Model completion request routed through the host's configured providers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCallRequest {
    /// Target model reference (`None` uses the runtime default).
    pub model: Option<ModelRef>,
    /// Optional system prompt text.
    pub system: Option<String>,
    /// Conversation messages to send.
    pub messages: Vec<Message>,
    /// Optional tool schemas advertised to the model.
    pub tools: Vec<ToolSchema>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Optional maximum output token limit.
    pub max_tokens: Option<u32>,
    /// Optional reasoning effort label.
    pub reasoning_effort: Option<String>,
    /// Optional JSON schema constraining structured output.
    pub json_schema: Option<Value>,
    /// Optional billing/accounting purpose tag.
    pub purpose: Option<UsagePurpose>,
}

/// A tool call emitted by a model completion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelToolCall {
    /// Provider tool call identifier.
    pub id: ToolCallId,
    /// Requested canonical tool name.
    pub name: ToolName,
    /// Parsed JSON arguments object.
    pub arguments: Value,
}

/// Completed non-streaming model response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCallResponse {
    /// Resolved serving model reference.
    pub model: ModelRef,
    /// Assistant visible text.
    pub text: String,
    /// Optional reasoning text, when returned by the route.
    pub reasoning: Option<String>,
    /// Tool calls emitted by the model.
    pub tool_calls: Vec<ModelToolCall>,
    /// Terminal finish reason.
    pub finish_reason: FinishReason,
    /// Token usage metrics, when reported.
    pub usage: Option<TokenUsage>,
}

/// Incremental chunk from a streaming model request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ModelStreamChunk {
    /// Appended visible text delta.
    TextDelta(String),
    /// Appended reasoning delta.
    ReasoningDelta(String),
    /// Emitted tool call.
    ToolCall(ModelToolCall),
    /// Token usage update.
    Usage(TokenUsage),
    /// Terminal stream completion.
    Finished(FinishReason),
}

/// Model context window capabilities reported by the host's provider route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWindowInfo {
    /// Maximum context window in tokens.
    pub max_context: u32,
    /// Whether the route reports token usage.
    pub usage_reporting: bool,
    /// Whether the route transmits tool-result images.
    pub sends_tool_images: bool,
}

/// Direct and streaming model invocation routed through the host.
#[async_trait]
pub trait HostModelCaller: Send + Sync {
    /// Execute a single non-streaming completion request.
    async fn complete(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError>;

    /// Start a streaming completion request and return a receiver of chunks.
    async fn stream(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError>;

    /// Invoke route-native compaction (`/responses/compact`) when available.
    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
    ) -> Result<Option<String>, HostError>;

    /// Inspect window capabilities for `model`, if known.
    async fn model_window(&self, model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError>;
}

// ---------------------------------------------------------------------------
// HostEventBus
// ---------------------------------------------------------------------------

/// Live event publication and subscription bus.
#[async_trait]
pub trait HostEventBus: Send + Sync {
    /// Publish an [`Envelope`] onto the live event bus.
    fn publish(&self, envelope: Envelope) -> Result<(), HostError>;

    /// Subscribe to live [`Envelope`] broadcasts.
    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError>;

    /// Convenience helper to emit a live extension event for `(session, bundle, kind)`.
    async fn publish_extension(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), HostError>;
}

// ---------------------------------------------------------------------------
// HostCatalogReader
// ---------------------------------------------------------------------------

/// Summary metadata for a bundle known to the host catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleCatalogSummary {
    /// Bundle identity block.
    pub identity: BundleIdentity,
    /// Whether this bundle is enabled in the active profile/config.
    pub enabled: bool,
    /// Optional bundle description.
    pub description: Option<String>,
}

/// Read-only access to agents, skills, bundles, and effective configuration.
#[async_trait]
pub trait HostCatalogReader: Send + Sync {
    /// List agents visible to `session` (or global catalog when `session` is `None`).
    async fn list_agents(&self, session: Option<SessionId>) -> Result<Vec<AgentDef>, HostError>;

    /// Fetch a prepared agent definition by name.
    async fn get_agent(
        &self,
        name: &AgentName,
        session: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError>;

    /// List skills visible to `session` (or global catalog when `session` is `None`).
    async fn list_skills(
        &self,
        session: Option<SessionId>,
    ) -> Result<Vec<SkillCatalogEntry>, HostError>;

    /// List bundles known to the host catalog.
    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError>;

    /// Load a prepared [`Bundle`] by id.
    async fn get_bundle(&self, id: &str) -> Result<Option<Bundle>, HostError>;

    /// Read effective per-bundle configuration JSON (`~/.config/hya/bundles/<id>/config.yml`).
    async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError>;

    /// Read effective global configuration JSON (`config.yaml`).
    async fn global_config(&self) -> Result<Value, HostError>;
}

// ---------------------------------------------------------------------------
// HostExtensionRegistry
// ---------------------------------------------------------------------------

/// Category of a registered native or JS extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    /// Model-callable tool.
    Tool,
    /// Context rewriter extension.
    Rewriter,
    /// Long-lived managed native service.
    Service,
    /// Runtime lifecycle/execution hook.
    Hook,
    /// Bundle API endpoint handler.
    Api,
    /// Event-log projection reducer.
    ProjectionReducer,
    /// Custom URI scheme handler (`scheme://`).
    UriScheme,
    /// Goal iteration driver.
    GoalDriver,
    /// Loop iteration driver.
    LoopDriver,
}

/// Descriptor for one registered extension item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    /// Owning bundle id.
    pub bundle_id: String,
    /// Extension kind.
    pub kind: ExtensionKind,
    /// Extension name, endpoint id, scheme, or hook id.
    pub name: String,
    /// Extension-specific metadata JSON.
    pub metadata: Value,
}

/// Request parameters for invoking a bundle API endpoint through the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleApiInvocation {
    /// Target bundle id.
    pub bundle: String,
    /// HTTP method.
    pub method: ApiMethod,
    /// Optional session scope.
    pub session: Option<SessionId>,
    /// Request path below the bundle mount.
    pub path: String,
    /// Query parameters.
    pub query: BTreeMap<String, String>,
    /// JSON request body (`Value::Null` when absent).
    pub body: Value,
}

/// Result returned by invoking a bundle API endpoint through the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleApiResult {
    /// HTTP-style status code (`200..=599`).
    pub status: u16,
    /// JSON response body.
    pub body: Value,
}

/// Query and cross-invoke registered tools, bundle APIs, and extensions.
#[async_trait]
pub trait HostExtensionRegistry: Send + Sync {
    /// List registered extensions, optionally filtered by [`ExtensionKind`].
    async fn list_extensions(
        &self,
        kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError>;

    /// Return `true` if `bundle_id` is currently loaded and active.
    async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError>;

    /// Invoke an endpoint on a loaded bundle's API surface.
    async fn invoke_bundle_api(
        &self,
        call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError>;

    /// Invoke a registered tool by canonical name.
    async fn invoke_tool(
        &self,
        session: Option<SessionId>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<Value, HostError>;

    /// Return `true` when at least one `goal.evaluate` hook provider is
    /// registered for `session` (or globally when `session` is `None`).
    async fn has_goal_evaluate_hook(&self, _session: Option<SessionId>) -> Result<bool, HostError> {
        Ok(false)
    }

    /// Dispatch the `goal.evaluate` hook chain for `session`, returning
    /// `Ok(None)` when no `goal.evaluate` provider is registered.
    async fn invoke_goal_evaluate_hook(
        &self,
        _session: Option<SessionId>,
        _condition: &str,
        _transcript: &str,
    ) -> Result<Option<NativeGoalEvaluateVerdict>, HostError> {
        Ok(None)
    }

    /// Consult registered `loop.should_stop` hooks for `session` (`Ok(None)` when none stop).
    async fn invoke_loop_should_stop(
        &self,
        _session: SessionId,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    /// Invoke registered `loop.verifier` hooks for `session` (`Ok(None)` when unregistered).
    async fn invoke_loop_verify(
        &self,
        _session: SessionId,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<NativeVerifierVerdict>, HostError> {
        Ok(None)
    }

    /// Invoke registered `loop.planner` hooks for `session` (`Ok(None)` when unregistered).
    async fn invoke_loop_plan(
        &self,
        _session: SessionId,
        _input: NativeLoopPlanInput,
    ) -> Result<Option<NativePlannerOutput>, HostError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// HostRuntime
// ---------------------------------------------------------------------------

/// Unified host runtime interface injected into native ABI v2 bundles.
///
/// Each method returns a reference to one of the seven host capability planes,
/// allowing native bundles to interact with sessions, KV/claim storage, turn
/// execution, models, the event bus, catalogs, and sibling extensions without
/// linking internal host crates.
pub trait HostRuntime: Send + Sync {
    /// Access the session event log and projection store.
    fn session_store(&self) -> &dyn HostSessionStore;

    /// Access generic bundle KV (`bundle_state`) and lease (`bundle_claim`) storage.
    fn bundle_storage(&self) -> &dyn HostBundleStorage;

    /// Access session creation and turn execution.
    fn turn_runner(&self) -> &dyn HostTurnRunner;

    /// Access model completion and streaming routes.
    fn model_caller(&self) -> &dyn HostModelCaller;

    /// Access the live event publication and subscription bus.
    fn event_bus(&self) -> &dyn HostEventBus;

    /// Access read-only agent, skill, bundle, and config catalogs.
    fn catalog_reader(&self) -> &dyn HostCatalogReader;

    /// Query and invoke registered extensions and bundle APIs.
    fn extension_registry(&self) -> &dyn HostExtensionRegistry;

    /// Look up the active [`GoalDriver`] registered by a loaded native bundle
    /// (such as `hya/goal`), returning `None` when disabled or in `--bare` mode.
    fn goal_driver(&self) -> Option<Arc<dyn GoalDriver>> {
        None
    }

    /// Return the active [`LoopDriver`] registered by `hya/loop`, if loaded.
    fn loop_driver(&self) -> Option<Arc<dyn LoopDriver>> {
        None
    }
}
