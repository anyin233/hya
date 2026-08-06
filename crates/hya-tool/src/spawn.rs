//! Subagent spawn plane: request transport used by the `task` tool.

use std::sync::Arc;

use hya_proto::SessionId;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{AgentDef, tool::ToolOperation};

/// One member of a `task` spawn request.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SpawnMember {
    /// Short label for UI and mail.
    pub description: String,
    /// Work prompt for the child agent.
    pub prompt: String,
    /// Agent id / subagent type to spawn.
    pub subagent_type: String,
    /// Optional existing subagent session to resume.
    pub task_id: Option<String>,
    /// Spawn-time explicit model override (highest precedence). `None`/empty
    /// defers down the Bundle definition / request-overlay model chain.
    pub model: Option<String>,
    /// Spawn-time logical category override. `None`/empty defers to inline and
    /// Bundle definition category layers, then the base model.
    pub category: Option<String>,
    /// Request-scoped agent overlay for this spawn only. Supplies system
    /// prompt + name and folds into the model/category precedence chain; never
    /// a catalog or Bundle definition authority.
    pub inline_agent: Option<InlineAgent>,
    /// Spawn-time opt-in to the resident (long-lived actor) lifecycle (ADR-0002).
    /// OR'd with Bundle definition and request-scoped inline `resident` — `true`
    /// from any source makes the member resident. Default `false` (transient).
    pub resident: bool,
}

/// Request-scoped agent overlay attached to a single spawn.
///
/// Carries name, system prompt, and optional `category`/`model` for this child
/// only. It is not retained as a Bundle definition or catalog entry.
#[derive(Clone, Debug, Default, Serialize)]
pub struct InlineAgent {
    /// Human-friendly agent name (used as the spawned session's agent name).
    pub name: String,
    /// The system prompt / persona for the ephemeral agent.
    pub prompt: String,
    /// Optional short description on the request overlay (not a Bundle field).
    pub description: Option<String>,
    /// Logical model category (request-overlay layer in spawn model precedence).
    pub category: Option<String>,
    /// Concrete `provider/model` (request-overlay layer in spawn model precedence).
    pub model: Option<String>,
    /// Request-scoped opt-in to the resident lifecycle.
    pub resident: Option<bool>,
}

/// Result for one spawned member after the host finishes (or admits) the spawn.
#[derive(Clone, Debug)]
pub struct MemberOutcome {
    /// Member label echoed from the request.
    pub member: String,
    /// Child session id as a string.
    pub session: String,
    /// Lifecycle status string (`done`, `failed`, …).
    pub status: String,
    /// Short summary text for the parent tool result.
    pub summary: String,
}

/// Host-bound request carrying parent context and a reply channel.
pub struct SpawnRequest {
    /// Parent session that owns the spawn.
    pub parent: SessionId,
    /// Authorized agent roster for `can_spawn` checks.
    pub agents: Arc<[AgentDef]>,
    /// Immutable triggering-turn guidance captured by the parent turn.
    /// Request-scoped Arc clone only; never discovered on the child workdir.
    pub guidance: Option<Arc<str>>,
    /// Operation identity of the triggering tool call.
    pub operation: ToolOperation,
    /// Members to spawn.
    pub members: Vec<SpawnMember>,
    /// Cancellation for the spawn work.
    pub cancel: CancellationToken,
    /// When true, host should not block the tool on completion.
    pub background: bool,
    /// Reply with per-member outcomes or a spawn error.
    pub reply: oneshot::Sender<Result<Vec<MemberOutcome>, SpawnError>>,
}

/// Failure admitting or running a spawn request.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SpawnError {
    /// Plane has no session or the sink is gone.
    #[error("spawner channel unavailable")]
    Unavailable,
    /// Admission queue is full.
    #[error("spawn admission overloaded")]
    Overloaded,
    /// Operation id conflicts with an in-flight claim.
    #[error("OPERATION_ID_CONFLICT")]
    OperationIdConflict,
    /// Operation id was already completed.
    #[error("operation already handled")]
    OperationAlreadyHandled,
    /// Cancelled before the child activated.
    #[error("spawn cancelled before activation")]
    Cancelled,
    /// Unknown agent type relative to the roster.
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId {
        /// Requested agent id.
        agent_id: String,
    },
    /// Roster forbids this caller/target pair.
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed {
        /// Calling agent id.
        caller: String,
        /// Target agent id.
        agent_id: String,
    },
    /// Inline overlay field not supported by the host.
    #[error("UNSUPPORTED_INLINE_AGENT_FIELD: `{field}`")]
    UnsupportedInlineAgentField {
        /// Unsupported field name.
        field: &'static str,
    },
}

/// Non-blocking sink error when enqueueing a [`SpawnRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnRequestSendError {
    /// Bounded queue is full.
    Full,
    /// Receiver dropped.
    Closed,
}

/// Narrow request transport used by [`SpawnerPlane`].
///
/// The default implementation writes raw [`SpawnRequest`] values to a bounded
/// Tokio channel. Runtime owners can supply a typed sink that enriches the
/// request without introducing a dependency from `hya-tool` back to them.
pub trait SpawnRequestSink: Send + Sync {
    /// Hand `request` to the runtime without blocking.
    ///
    /// Returns [`SpawnRequestSendError::Full`] when the sink is at capacity and
    /// [`SpawnRequestSendError::Closed`] once the receiving runtime is gone. Both
    /// are non-fatal to the caller: the `task` tool surfaces them as a tool error
    /// rather than tearing down the session.
    fn try_send(&self, request: SpawnRequest) -> Result<(), SpawnRequestSendError>;
}

struct ChannelSpawnRequestSink {
    tx: mpsc::Sender<SpawnRequest>,
}

impl SpawnRequestSink for ChannelSpawnRequestSink {
    fn try_send(&self, request: SpawnRequest) -> Result<(), SpawnRequestSendError> {
        self.tx.try_send(request).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SpawnRequestSendError::Full,
            mpsc::error::TrySendError::Closed(_) => SpawnRequestSendError::Closed,
        })
    }
}

/// Session-scoped facade tools use to request subagent spawns.
#[derive(Clone)]
pub struct SpawnerPlane {
    sink: Arc<dyn SpawnRequestSink>,
    session: Option<SessionId>,
    agents: Arc<[AgentDef]>,
    /// Immutable guidance captured for the parent turn; Arc-cloned onto each spawn.
    guidance: Option<Arc<str>>,
}

impl SpawnerPlane {
    /// Build a minimally buffered plane for disconnected engines and focused tests.
    ///
    /// Product runtime wiring should use [`Self::with_capacity`] with its existing
    /// configured subagent budget.
    #[must_use]
    pub fn new() -> (Self, mpsc::Receiver<SpawnRequest>) {
        Self::with_capacity(1)
    }

    /// Create a channel-backed plane with the given admission capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, mpsc::Receiver<SpawnRequest>) {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (tx, rx) = mpsc::channel(capacity);
        let sink = Arc::new(ChannelSpawnRequestSink { tx });
        (Self::from_sink(sink), rx)
    }

    /// Build an unscoped plane over a runtime-owned request sink.
    #[must_use]
    pub fn from_sink(sink: Arc<dyn SpawnRequestSink>) -> Self {
        Self {
            sink,
            session: None,
            agents: Arc::from([]),
            guidance: None,
        }
    }

    /// Scope spawns to a parent session (roster empty until set).
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    /// Scope this plane to the caller and the spawn graph captured by its turn.
    #[must_use]
    pub fn for_session_with_agents(&self, session: SessionId, agents: Arc<[AgentDef]>) -> Self {
        let mut plane = self.for_session(session);
        plane.agents = agents;
        plane
    }

    /// Scope session, authorized roster, and immutable triggering-turn guidance.
    ///
    /// Fields are assigned directly on a session-scoped clone (no intermediate
    /// public convenience setter). Guidance is Arc-cloned onto each spawn request.
    #[must_use]
    pub fn for_session_with_agents_and_guidance(
        &self,
        session: SessionId,
        agents: Arc<[AgentDef]>,
        guidance: Option<Arc<str>>,
    ) -> Self {
        let mut plane = self.for_session(session);
        plane.agents = agents;
        plane.guidance = guidance;
        plane
    }

    /// Spawn members and wait for host outcomes (foreground).
    ///
    /// # Errors
    /// Returns [`SpawnError`] when admission fails or the host reports an error.
    pub async fn spawn(
        &self,
        operation: ToolOperation,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        self.spawn_inner(operation, members, cancel, false).await
    }

    /// Spawn members with `background = true` (host may return early).
    ///
    /// # Errors
    /// Returns [`SpawnError`] when admission fails or the host reports an error.
    pub async fn spawn_background(
        &self,
        operation: ToolOperation,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        self.spawn_inner(operation, members, cancel, true).await
    }

    async fn spawn_inner(
        &self,
        operation: ToolOperation,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
        background: bool,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        let parent = self.session.ok_or(SpawnError::Unavailable)?;
        let (tx, rx) = oneshot::channel();
        let req = SpawnRequest {
            parent,
            agents: self.agents.clone(),
            guidance: self.guidance.clone(),
            operation,
            members,
            cancel,
            background,
            reply: tx,
        };
        self.sink.try_send(req).map_err(|error| match error {
            SpawnRequestSendError::Full => SpawnError::Overloaded,
            SpawnRequestSendError::Closed => SpawnError::Unavailable,
        })?;
        rx.await.map_err(|_| SpawnError::Unavailable)?
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::ToolCallId;

    fn operation() -> ToolOperation {
        ToolOperation::from_tool_call(ToolCallId::new())
    }

    #[tokio::test]
    async fn spawn_round_trips_outcomes() {
        let (plane, mut rx) = SpawnerPlane::new();
        let plane = plane.for_session(SessionId::new());
        let task = tokio::spawn(async move {
            plane
                .spawn(
                    operation(),
                    vec![SpawnMember {
                        description: "d".to_string(),
                        prompt: "p".to_string(),
                        subagent_type: "quick".to_string(),
                        task_id: None,
                        model: None,
                        category: None,
                        inline_agent: None,
                        resident: false,
                    }],
                    CancellationToken::new(),
                )
                .await
        });
        let req = rx.recv().await.expect("request");
        assert_eq!(req.members.len(), 1);
        req.reply
            .send(Ok(vec![MemberOutcome {
                member: "m1".to_string(),
                session: "s1".to_string(),
                status: "done".to_string(),
                summary: "ok".to_string(),
            }]))
            .expect("reply");
        let outcomes = task.await.expect("join").expect("outcomes");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, "done");
    }

    #[tokio::test]
    async fn spawn_without_session_is_unavailable() {
        let (plane, _rx) = SpawnerPlane::new();
        let result = plane
            .spawn(operation(), Vec::new(), CancellationToken::new())
            .await;
        assert!(matches!(result, Err(SpawnError::Unavailable)));
    }

    #[tokio::test]
    async fn bound_spawn_with_closed_receiver_is_unavailable() {
        let (plane, rx) = SpawnerPlane::new();
        drop(rx);

        let result = plane
            .for_session(SessionId::new())
            .spawn(
                operation(),
                vec![SpawnMember::default()],
                CancellationToken::new(),
            )
            .await;

        assert!(matches!(result, Err(SpawnError::Unavailable)));
    }

    #[test]
    fn spawn_queue_capacity_is_clamped_to_tokio_limit() {
        let (_plane, rx) = SpawnerPlane::with_capacity(usize::MAX);
        assert_eq!(rx.max_capacity(), tokio::sync::Semaphore::MAX_PERMITS);
    }

    #[tokio::test]
    async fn full_spawn_queue_fails_fast_with_overload() {
        let (plane, rx) = SpawnerPlane::new();
        let queued_plane = plane.for_session(SessionId::new());
        let queued = tokio::spawn(async move {
            queued_plane
                .spawn(
                    operation(),
                    vec![SpawnMember::default()],
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rx.len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first request was not queued");

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            plane.for_session(SessionId::new()).spawn(
                operation(),
                vec![SpawnMember::default()],
                CancellationToken::new(),
            ),
        )
        .await
        .expect("full spawn queue must fail fast");

        queued.abort();
        assert!(matches!(result, Err(SpawnError::Overloaded)));
        assert_eq!(rx.len(), 1, "overloaded request must not enter the queue");
    }
}
