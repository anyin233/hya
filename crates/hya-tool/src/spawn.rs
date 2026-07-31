use hya_proto::SessionId;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::tool::ToolOperation;

#[derive(Clone, Debug, Default, Serialize)]
pub struct SpawnMember {
    pub description: String,
    pub prompt: String,
    pub subagent_type: String,
    pub task_id: Option<String>,
    /// Spawn-time explicit model override (precedence 1, highest). `None`/empty
    /// defers to the agent's own model/category.
    pub model: Option<String>,
    /// Spawn-time logical category override (precedence 2). `None`/empty defers
    /// to the agent's frontmatter category, then the global default.
    pub category: Option<String>,
    /// A full inline agent definition that lives only for this spawn (no disk
    /// write). When present it supplies the system prompt + name and folds into
    /// the same model/category precedence chain (decision 11).
    pub inline_agent: Option<InlineAgent>,
    /// Spawn-time opt-in to the resident (long-lived actor) lifecycle (ADR-0002).
    /// OR'd with the agent's frontmatter/inline `resident:` — `true` from any
    /// source makes the member resident. Default `false` (transient, unchanged).
    pub resident: bool,
}

/// A runtime-authored, ephemeral agent definition attached to a single spawn.
///
/// It carries the same core fields a disk agent's frontmatter would (name,
/// system prompt, optional `category`/`model`) but is never persisted; an agent
/// that wants reuse saves an `.md` itself via the existing `write` tool.
#[derive(Clone, Debug, Default, Serialize)]
pub struct InlineAgent {
    /// Human-friendly agent name (used as the spawned session's agent name).
    pub name: String,
    /// The system prompt / persona for the ephemeral agent.
    pub prompt: String,
    /// Optional short description (parity with frontmatter `description:`).
    pub description: Option<String>,
    /// Logical model category (`~` frontmatter `category:` in precedence).
    pub category: Option<String>,
    /// Concrete `provider/model` (`~` frontmatter `model:` in precedence).
    pub model: Option<String>,
    /// Inline opt-in to the resident lifecycle (`~` frontmatter `resident:`).
    pub resident: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct MemberOutcome {
    pub member: String,
    pub session: String,
    pub status: String,
    pub summary: String,
}

pub struct SpawnRequest {
    pub parent: SessionId,
    pub operation: ToolOperation,
    pub members: Vec<SpawnMember>,
    pub cancel: CancellationToken,
    pub background: bool,
    pub reply: oneshot::Sender<Result<Vec<MemberOutcome>, SpawnError>>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SpawnError {
    #[error("spawner channel unavailable")]
    Unavailable,
    #[error("spawn admission overloaded")]
    Overloaded,
    #[error("OPERATION_ID_CONFLICT")]
    OperationIdConflict,
    #[error("operation already handled")]
    OperationAlreadyHandled,
}

#[derive(Clone)]
pub struct SpawnerPlane {
    tx: mpsc::Sender<SpawnRequest>,
    session: Option<SessionId>,
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

    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, mpsc::Receiver<SpawnRequest>) {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx, session: None }, rx)
    }

    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    pub async fn spawn(
        &self,
        operation: ToolOperation,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        self.spawn_inner(operation, members, cancel, false).await
    }

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
            operation,
            members,
            cancel,
            background,
            reply: tx,
        };
        self.tx.try_send(req).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SpawnError::Overloaded,
            mpsc::error::TrySendError::Closed(_) => SpawnError::Unavailable,
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
