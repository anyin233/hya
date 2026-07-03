use hya_proto::SessionId;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default)]
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
}

/// A runtime-authored, ephemeral agent definition attached to a single spawn.
///
/// It carries the same core fields a disk agent's frontmatter would (name,
/// system prompt, optional `category`/`model`) but is never persisted; an agent
/// that wants reuse saves an `.md` itself via the existing `write` tool.
#[derive(Clone, Debug, Default)]
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
    pub members: Vec<SpawnMember>,
    pub cancel: CancellationToken,
    pub background: bool,
    pub reply: oneshot::Sender<Vec<MemberOutcome>>,
}

#[derive(Error, Debug)]
pub enum SpawnError {
    #[error("spawner channel unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct SpawnerPlane {
    tx: mpsc::UnboundedSender<SpawnRequest>,
    session: Option<SessionId>,
}

impl SpawnerPlane {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<SpawnRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
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
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        self.spawn_inner(members, cancel, false).await
    }

    pub async fn spawn_background(
        &self,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        self.spawn_inner(members, cancel, true).await
    }

    async fn spawn_inner(
        &self,
        members: Vec<SpawnMember>,
        cancel: CancellationToken,
        background: bool,
    ) -> Result<Vec<MemberOutcome>, SpawnError> {
        let parent = self.session.ok_or(SpawnError::Unavailable)?;
        let (tx, rx) = oneshot::channel();
        let req = SpawnRequest {
            parent,
            members,
            cancel,
            background,
            reply: tx,
        };
        self.tx.send(req).map_err(|_| SpawnError::Unavailable)?;
        rx.await.map_err(|_| SpawnError::Unavailable)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_round_trips_outcomes() {
        let (plane, mut rx) = SpawnerPlane::new();
        let plane = plane.for_session(SessionId::new());
        let task = tokio::spawn(async move {
            plane
                .spawn(
                    vec![SpawnMember {
                        description: "d".to_string(),
                        prompt: "p".to_string(),
                        subagent_type: "quick".to_string(),
                        task_id: None,
                        model: None,
                        category: None,
                        inline_agent: None,
                    }],
                    CancellationToken::new(),
                )
                .await
        });
        let req = rx.recv().await.expect("request");
        assert_eq!(req.members.len(), 1);
        req.reply
            .send(vec![MemberOutcome {
                member: "m1".to_string(),
                session: "s1".to_string(),
                status: "done".to_string(),
                summary: "ok".to_string(),
            }])
            .expect("reply");
        let outcomes = task.await.expect("join").expect("outcomes");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, "done");
    }

    #[tokio::test]
    async fn spawn_without_session_is_unavailable() {
        let (plane, _rx) = SpawnerPlane::new();
        let result = plane.spawn(Vec::new(), CancellationToken::new()).await;
        assert!(matches!(result, Err(SpawnError::Unavailable)));
    }
}
