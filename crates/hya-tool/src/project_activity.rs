#![allow(missing_docs)]
//! Read-only project activity plane.
use hya_proto::SessionId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ProjectActivityRequest {
    pub since_ms: Option<i64>,
    pub limit: Option<usize>,
    pub include_self: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectActivitySession {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub status: String,
    pub last_activity_ms: i64,
    pub workdir: String,
    pub parent: Option<String>,
    pub lineage_root: String,
    pub relation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectActivityFile {
    pub path: String,
    pub session: String,
    pub last_changed_ms: i64,
    pub change_kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectActivityResult {
    pub sessions: Vec<ProjectActivitySession>,
    pub files: Vec<ProjectActivityFile>,
    pub note: Option<String>,
}

pub enum ProjectActivityRequestMsg {
    Query {
        session: SessionId,
        request: ProjectActivityRequest,
        reply: oneshot::Sender<Result<ProjectActivityResult, String>>,
    },
}

#[derive(Debug, Error)]
pub enum ProjectActivityError {
    #[error("project activity service unavailable")]
    Unavailable,
    #[error("{0}")]
    Rejected(String),
}

#[derive(Clone, Default)]
pub struct ProjectActivityPlane {
    tx: Option<mpsc::UnboundedSender<ProjectActivityRequestMsg>>,
    session: Option<SessionId>,
}

impl ProjectActivityPlane {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ProjectActivityRequestMsg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: Some(tx),
                session: None,
            },
            rx,
        )
    }

    #[must_use]
    pub fn disconnected() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    pub async fn query(
        &self,
        request: ProjectActivityRequest,
    ) -> Result<ProjectActivityResult, ProjectActivityError> {
        let tx = self.tx.as_ref().ok_or(ProjectActivityError::Unavailable)?;
        let session = self.session.ok_or(ProjectActivityError::Unavailable)?;
        let (reply, rx) = oneshot::channel();
        tx.send(ProjectActivityRequestMsg::Query {
            session,
            request,
            reply,
        })
        .map_err(|_| ProjectActivityError::Unavailable)?;
        rx.await
            .map_err(|_| ProjectActivityError::Unavailable)?
            .map_err(ProjectActivityError::Rejected)
    }
}
