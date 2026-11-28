//! Host runtime capability traits injected into native bundles via ABI v2.
//!
//! Native bundle dynamic libraries must never depend on `hya-store`, `sqlx`,
//! `hya-engine`, `hya-server`, `axum`, or `tonic`. Instead, the host injects an
//! `Arc<dyn HostRuntime>` providing typed access to session logs, generic bundle
//! KV/claim storage, turn execution, model calls, the event bus, catalogs, and
//! registered extensions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, Bundle, BundleIdentity, PreparedAgent};
use hya_proto::{
    ActorClaim, AgentName, ChannelPolicySnapshot, Envelope, Event, EventSeq, FinishReason,
    MemberId, Message, MessageId, ModelRef, OwnerRunId, ProjectId, Projection, SessionId,
    TokenUsage, ToolCallId, ToolName, ToolSchema, UsagePurpose,
};
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::error::HostError;

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

    /// Stable process-instance owner id used to detect stale runs across restarts.
    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        None
    }

    /// Find all sessions whose durable event log contains an `Event::Extension`
    /// matching `(bundle, kind)`.
    async fn find_sessions_with_extension(
        &self,
        _bundle: &str,
        _kind: &str,
    ) -> Result<Vec<SessionId>, HostError> {
        Ok(Vec::new())
    }

    /// Stage an `Event::Extension` for `target`'s log that commits in the same
    /// store transaction as the tool result `during` is producing.
    ///
    /// Call it from a `tool.execute.after` hook. When the result commits, the
    /// host runs `on_commit` after the commit, in staging order; otherwise it
    /// drops the event and `on_commit`. Returns `InvalidInput` when `during`
    /// has no tool result in its after-hooks.
    fn stage_extension_event(
        &self,
        _during: SessionId,
        _target: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
        _on_commit: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "staged extension events are unavailable".into(),
        ))
    }

    /// Append `events` (each to its own session log) in one transaction.
    ///
    /// The batch commits only when `fence` (if any) is the current holder of
    /// its session fence and each `(session, seq)` in `expect` is still that
    /// session's last sequence; otherwise nothing is written and the call
    /// returns [`HostError::StaleClaim`] or [`HostError::Conflict`].
    async fn append_batch(
        &self,
        _fence: Option<ActorClaim>,
        _expect: Vec<(SessionId, EventSeq)>,
        _events: Vec<(SessionId, Event)>,
    ) -> Result<Vec<Envelope>, HostError> {
        Err(HostError::Unavailable("batch append is unavailable".into()))
    }
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
    /// Read a process-local JSON value in a bundle's session scope.
    fn get_transient_state(
        &self,
        _bundle: &str,
        _session: SessionId,
        _key: &str,
    ) -> Result<Option<Value>, HostError> {
        Err(HostError::Unavailable(
            "transient state is unavailable".into(),
        ))
    }

    /// Set a process-local value, or remove it with `None`.
    fn set_transient_state(
        &self,
        _bundle: &str,
        _session: SessionId,
        _key: &str,
        _value: Option<Value>,
    ) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "transient state is unavailable".into(),
        ))
    }

    /// Watch one key's current value and subsequent changes, including removal.
    fn subscribe_transient_state(
        &self,
        _bundle: &str,
        _session: SessionId,
        _key: &str,
    ) -> Result<tokio::sync::watch::Receiver<Option<Value>>, HostError> {
        Err(HostError::Unavailable(
            "transient state is unavailable".into(),
        ))
    }

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

    /// List all active (non-expired) claims owned by `bundle`.
    async fn list_claims(&self, _bundle: &str) -> Result<Vec<BundleClaimLease>, HostError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// HostTurnRunner
// ---------------------------------------------------------------------------

/// Request to execute a single turn on an existing session.
///
/// A fenced turn (`actor_claim: Some`) runs as a child of its parent's
/// binding (a session without a parent is [`HostError::InvalidInput`]): the
/// fence is a `hya/multi-agent` claim keyed by the session id, and the host
/// admits the prompt under it, resolves the agent, its system prompt, roster, and
/// resource policy from the parent, re-validates the fence before each round
/// and before opening each stream, fences every append, and keeps one
/// sidecar for the session from its first fenced turn until the fence is
/// released or the process stops. A lost sidecar fails the running turn with
/// [`HostError::Unavailable`]. An unfenced turn binds the session itself and
/// starts a sidecar per turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStartRequest {
    /// Target session id.
    pub session: SessionId,
    /// User/directive prompt admitted before the turn; `None` runs on the
    /// log tail the caller appended.
    pub prompt: Option<String>,
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
    /// Guidance layer composed into the system prompt for this turn.
    pub guidance: Option<String>,
    /// Explicit model route resolved with [`HostModelCaller::resolve_model_route`].
    pub route: Option<ResolvedModelRoute>,
    /// Whether the turn queues behind an active turn or starts only when idle.
    pub start: TurnStart,
}

/// How [`HostTurnRunner::run_turn`] takes the session's single turn lease.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStart {
    /// Wait for an active turn to end, then run.
    #[default]
    Always,
    /// Take the lease atomically, or return [`HostError::Conflict`] without
    /// admitting the prompt when a turn is active; `run_turn` returns
    /// `Conflict` for no other reason.
    IfIdle,
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

/// Failure classification recorded when an explicit turn model route advances or fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRouteFailureClass {
    /// Selected candidate succeeded with no failure.
    None,
    /// Retryable transport failure before a stream was established.
    Transport,
    /// Upstream rate limiting (`429`) before a stream was established.
    RateLimited,
    /// Retryable upstream server error (`5xx` or overloaded).
    Server,
    /// No provider route claimed the candidate model.
    UnknownModel,
    /// Authentication or authorization failure.
    Auth,
    /// Provider capability or request incompatibility.
    Incompatible,
    /// Non-retryable HTTP/provider response failure.
    Http,
    /// Malformed or truncated provider stream.
    Decode,
    /// Internal invariant, store, or claim failure after an attempt started.
    Internal,
    /// Sidecar loss or task abort after an attempt started.
    Aborted,
    /// Explicit cancellation.
    Cancelled,
}

/// One candidate in an ordered explicit model route request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRouteEntry {
    /// Base model reference (without effort suffix).
    pub model: ModelRef,
    /// Optional authored reasoning effort label (e.g. `"low"`, `"medium"`, `"high"`).
    pub reasoning: Option<String>,
}

/// One host-validated candidate in a resolved explicit model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModelRouteCandidate {
    /// Base model reference.
    pub model: ModelRef,
    /// Canonical reasoning effort string (e.g. `"off"`, `"medium"`, `"high"`).
    pub reasoning: String,
}

/// Ordered explicit model route validated against the host's provider router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModelRoute {
    /// Full declaration-order candidates.
    pub candidates: Vec<ResolvedModelRouteCandidate>,
    /// Index of the first routable candidate at admission time.
    pub selected_index: usize,
}

impl ResolvedModelRoute {
    /// Return the candidate selected at admission time.
    #[must_use]
    pub fn selected(&self) -> Option<&ResolvedModelRouteCandidate> {
        self.candidates.get(self.selected_index)
    }
}

/// One finalized candidate attempt outcome emitted during a routed turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRouteOutcome {
    /// Zero-based stream-group step within the activation.
    pub step: u32,
    /// Zero-based index into [`ResolvedModelRoute::candidates`].
    pub candidate_index: u32,
    /// Model reference that handled or terminated the stream group.
    pub model: ModelRef,
    /// Canonical reasoning effort applied to that candidate.
    pub reasoning: String,
    /// Failure class causing a fallback advance or terminal failure, if any.
    pub failure_class: TurnRouteFailureClass,
}

/// Handle to a parked managed resident child member spawned under a parent session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedMemberHandle {
    /// Child session identifier.
    pub session: SessionId,
    /// Resident supervisor handle within the parent's root team.
    pub handle: String,
    /// Durable member identifier.
    pub member: MemberId,
}

/// Request to spawn a parked managed resident child member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedMemberSpawnRequest {
    /// Parent session spawning the member.
    pub parent: SessionId,
    /// Caller agent id authorizing the spawn (used for scope/policy validation).
    pub caller: AgentName,
    /// Target agent id to run inside the member.
    pub agent: AgentName,
    /// Human-readable display label for the spawned member.
    pub label: String,
    /// Optional explicit model route resolved via [`HostModelCaller::resolve_model_route`].
    pub route: Option<ResolvedModelRoute>,
    /// Optional initial system context guidance attached to the member.
    pub system_context: Option<String>,
    /// Optional actor claim fencing parent mutations.
    pub actor_claim: Option<ActorClaim>,
}

/// Request to activate a parked managed resident child member for one episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedMemberActivateRequest {
    /// Parent session owning the member.
    pub parent: SessionId,
    /// Previously spawned member handle.
    pub member: ManagedMemberHandle,
    /// Target agent id bound to the member.
    pub agent: AgentName,
    /// Directive prompt delivered to the member.
    pub directive: String,
    /// System context guidance for this activation.
    pub system_context: String,
    /// Optional explicit model route for this activation.
    pub route: Option<ResolvedModelRoute>,
    /// Optional actor claim fencing parent mutations.
    pub actor_claim: Option<ActorClaim>,
}

/// Terminal disposition of one managed resident member activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedMemberActivationStatus {
    /// Member settled or reported `Done`.
    Done,
    /// Member failed or reported a non-`Done` outcome.
    Failed,
    /// Activation was cancelled cooperatively.
    Cancelled,
}

/// Result of activating a managed resident child member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedMemberActivationOutcome {
    /// Terminal disposition.
    pub status: ManagedMemberActivationStatus,
    /// Final assistant output or error note.
    pub output_text: String,
    /// Ordered route attempt outcomes finalized during this activation.
    pub route_outcomes: Vec<TurnRouteOutcome>,
}

/// Request to revive an archived direct child member and deliver mail to its DM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberMailRevivalRequest {
    /// Team root session owning the channel log.
    pub root: SessionId,
    /// Sender session requesting delivery.
    pub from_session: SessionId,
    /// Canonical handle of the sender.
    pub from_handle: String,
    /// Canonical handle of the archived direct child.
    pub target_handle: String,
    /// Minted DM channel id between sender and target.
    pub channel: String,
    /// Message kind (`"message"` or `"announcement"`).
    pub kind: hya_proto::MailKind,
    /// Message body text.
    pub body: String,
    /// Optional actor claim fencing the sender's mutations.
    pub actor_claim: Option<ActorClaim>,
}

/// Result of archiving a session subtree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTreeArchive {
    /// Whether an active turn of the requested session was cancelled.
    pub cancelled_turn: bool,
    /// Sessions newly archived below the requested session, descendants first.
    pub descendants: Vec<SessionId>,
}

/// Session creation and turn-driving capability without linking `hya-engine`.
#[async_trait]
pub trait HostTurnRunner: Send + Sync {
    /// Create a session according to `request` without starting a turn.
    async fn spawn_session(&self, request: TurnSpawnRequest) -> Result<SessionId, HostError>;

    /// Return direct child IDs in creation order, including archived children.
    async fn list_child_sessions(&self, _parent: SessionId) -> Result<Vec<SessionId>, HostError> {
        Err(HostError::Unavailable("session tree is unavailable".into()))
    }

    /// Archive this session and its descendants.
    async fn archive_session_tree(
        &self,
        _root: SessionId,
    ) -> Result<SessionTreeArchive, HostError> {
        Err(HostError::Unavailable("session tree is unavailable".into()))
    }

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

    /// Preflight team depth and capacity budget for `wanted` activations under `parent`.
    // Adapter: removed in CHR-47.
    async fn preflight_member_capacity(
        &self,
        _parent: SessionId,
        _wanted: u64,
    ) -> Result<(), HostError> {
        Ok(())
    }

    /// Acquire a root-session driver activity guard keeping the resident team active
    /// until the returned handle is dropped.
    // Adapter: removed in CHR-47.
    async fn begin_root_activity(
        &self,
        _parent: SessionId,
    ) -> Result<Arc<dyn Send + Sync>, HostError> {
        Ok(Arc::new(()))
    }

    /// Spawn a parked managed resident child member under `request.parent`.
    // Adapter: removed in CHR-47.
    async fn spawn_managed_member(
        &self,
        _request: ManagedMemberSpawnRequest,
    ) -> Result<ManagedMemberHandle, HostError> {
        Err(HostError::Unavailable(
            "managed resident member execution is unavailable in this runtime".into(),
        ))
    }

    /// Activate a parked managed resident child member and await settlement.
    // Adapter: removed in CHR-47.
    async fn activate_managed_member(
        &self,
        _request: ManagedMemberActivateRequest,
        _cancel: CancellationToken,
    ) -> Result<ManagedMemberActivationOutcome, HostError> {
        Err(HostError::Unavailable(
            "managed resident member execution is unavailable in this runtime".into(),
        ))
    }

    /// End a managed resident child member and release its live capacity slot.
    // Adapter: removed in CHR-47.
    async fn end_managed_member(&self, _parent: SessionId, _handle: &str) -> Result<(), HostError> {
        Ok(())
    }

    /// Resolve the effective channel policy snapshot for `session`, if any.
    // Adapter: removed in CHR-47.
    async fn session_channel_policy(
        &self,
        _session: SessionId,
    ) -> Result<Option<ChannelPolicySnapshot>, HostError> {
        Ok(None)
    }

    /// Return the in-memory acknowledged inbox watermark for `session`, if tracked.
    // Adapter: removed in CHR-47.
    fn mail_mark(&self, _session: SessionId) -> Option<u64> {
        None
    }

    /// Advance the in-memory mail watermark for `session` and notify listeners.
    // Adapter: removed in CHR-47.
    fn notify_mail_consumed(&self, _session: SessionId, _mark: u64, _consumed_incoming: bool) {}

    /// Return `true` when `handle` under `root` is currently parked by an explicit
    /// user stop/cancellation and should not receive in-flight resident wakeups.
    // Adapter: removed in CHR-47.
    fn is_member_stopped_by_user(&self, _root: SessionId, _handle: &str) -> bool {
        false
    }

    /// Return the active fenced actor claim for the running turn on `session`, if any.
    // Adapter: removed in CHR-47.
    fn active_actor_claim(&self, _session: SessionId) -> Option<ActorClaim> {
        None
    }

    /// Revive an archived direct child member and deliver its pending DM mail.
    ///
    /// Returns the delivered recipient count (`1`) on success.
    // Adapter: removed in CHR-47.
    async fn revive_archived_member_with_mail(
        &self,
        _request: MemberMailRevivalRequest,
    ) -> Result<usize, HostError> {
        Err(HostError::Unavailable(
            "archived member revival is unavailable in this runtime".into(),
        ))
    }

    /// Archive a subagent session on the user's behalf, cancelling any active
    /// turn and archiving live descendants first.
    // Adapter: removed in CHR-47.
    async fn archive_subagent(
        &self,
        _child: SessionId,
    ) -> Result<hya_tool::ArchiveReceipt, HostError> {
        Err(HostError::InvalidInput(
            "subagents are not supervised by this process".into(),
        ))
    }

    /// Steer a subagent session with a user message delivered on its parent DM.
    // Adapter: removed in CHR-47.
    async fn steer_subagent(
        &self,
        _child: SessionId,
        _text: &str,
    ) -> Result<hya_tool::MailReceipt, HostError> {
        Err(HostError::Unavailable(
            "subagent steering is unavailable in this runtime".into(),
        ))
    }
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

    /// Validate an ordered explicit model route against configured providers and
    /// reasoning capabilities, returning the resolved candidates and start index.
    async fn resolve_model_route(
        &self,
        _entries: &[ModelRouteEntry],
    ) -> Result<ResolvedModelRoute, HostError> {
        Err(HostError::Unavailable(
            "explicit model route resolution is unavailable in this runtime".into(),
        ))
    }
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

/// Resolved scope-aware catalog snapshot for directory- or session-bound lookups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeCatalogSnapshot {
    /// Effective working directory for this scope.
    pub workdir: PathBuf,
    /// Registered project roots (empty outside a registered project).
    pub project_roots: Vec<PathBuf>,
    /// Active prepared bundles in this scope.
    pub bundles: Vec<Bundle>,
    /// Stable runtime fingerprint covering active catalogs/bundles.
    pub runtime_fingerprint: String,
    /// Sources the scope skipped, one line each (`installed bundle <id>:
    /// <error>`, `project bundle <dir>: <error>`), in load order.
    pub diagnostics: Vec<String>,
}

/// Inputs of [`HostCatalogReader::resolve_agent_model`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelRequest {
    /// Session whose binding (and agent model preferences) the child inherits.
    pub parent: SessionId,
    /// Agent the child session runs.
    pub agent: AgentName,
    /// Model the caller named, if any.
    pub model: Option<ModelRef>,
    /// Model category the caller named, if any.
    pub category: Option<String>,
    /// Reasoning effort the caller named, if any.
    pub effort: Option<String>,
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

    /// Resolve the active scope catalog snapshot for a session or directory.
    async fn resolve_scope_catalog(
        &self,
        _session: Option<SessionId>,
        _directory: Option<&Path>,
    ) -> Result<ScopeCatalogSnapshot, HostError> {
        Err(HostError::Unavailable(
            "scope catalog resolution is unavailable in this runtime".into(),
        ))
    }

    /// Resolve the model (with any `#effort` suffix) a child of
    /// `request.parent` running `request.agent` uses, with the spawn
    /// precedence of `task`: from lowest to highest, the base model, the
    /// agent's remembered preference, its category, its model, the request's
    /// category, and the request's model matched against the provider catalog.
    /// The parent's agent must be allowed to spawn `request.agent`.
    async fn resolve_agent_model(
        &self,
        _request: AgentModelRequest,
    ) -> Result<ModelRef, HostError> {
        Err(HostError::Unavailable(
            "agent model resolution is unavailable in this runtime".into(),
        ))
    }

    /// Authorize spawning `agent` from `caller` within `session`'s active scope.
    async fn authorize_agent_run(
        &self,
        _session: SessionId,
        _caller: &AgentName,
        _agent: &AgentName,
    ) -> Result<(), HostError> {
        Ok(())
    }
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
    /// Run driver.
    Driver,
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

    /// Whether any hook bound to `session` implements the bundle-declared
    /// hook point `point` (`<bundle-id>:<name>`).
    async fn has_hook(&self, session: Option<SessionId>, point: &str) -> Result<bool, HostError> {
        let _ = (session, point);
        Ok(false)
    }

    /// Dispatch the bundle-declared hook point `point` for `session`: the
    /// first implementer that answers wins, a failing implementer is skipped
    /// with a warning, and `Ok(None)` means no implementer answered.
    ///
    /// # Errors
    /// [`HostError::InvalidInput`] when no loaded bundle declares `point`.
    async fn invoke_hook(
        &self,
        session: Option<SessionId>,
        point: &str,
        input: Value,
    ) -> Result<Option<Value>, HostError> {
        let _ = (session, point, input);
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
}
