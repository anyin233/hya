//! Subagent lifecycle plane (ADR-0015): the `report` and `kill` tools ride a
//! narrow request channel to the resident supervisor, mirroring the mailbox
//! plane's dependency inversion (`hya-tool` never sees `CoreError`).

use hya_proto::{ReportOutcome, SessionId};
use tokio::sync::{mpsc, oneshot};

use crate::mailbox::ChannelPolicySnapshot;
use crate::tool::ToolError;

/// One lifecycle request from a tool call to the supervisor.
#[derive(Debug)]
pub enum LifecycleRequest {
    /// Accept a terminal report; archives once the actor is at rest.
    Report {
        /// Reporting session.
        session: SessionId,
        /// Terminal outcome.
        outcome: ReportOutcome,
        /// Result text for the parent.
        report: String,
        /// Channel policy captured by the admitted turn.
        channel_policy: Option<ChannelPolicySnapshot>,
        /// Gate rejection text or acceptance.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Force-kill one direct child by handle.
    Kill {
        /// Killing (parent) session.
        session: SessionId,
        /// Child handle.
        handle: String,
        /// Bounded reason, surfaced to the killer as the failure report.
        reason: String,
        /// Rejection text or confirmation.
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Session-scoped facade tools use to request lifecycle transitions.
#[derive(Clone)]
pub struct LifecyclePlane {
    tx: Option<mpsc::UnboundedSender<LifecycleRequest>>,
    session: Option<SessionId>,
    channel_policy: Option<ChannelPolicySnapshot>,
}

impl Default for LifecyclePlane {
    fn default() -> Self {
        Self::disconnected()
    }
}

impl LifecyclePlane {
    /// Build a connected plane plus the receiver the service loop drains.
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<LifecycleRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: Some(tx),
                session: None,
                channel_policy: None,
            },
            rx,
        )
    }

    /// A plane whose sends fail fast (tests, disconnected engines).
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            tx: None,
            session: None,
            channel_policy: None,
        }
    }

    /// Scope requests to the acting session.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    /// Attach the channel policy captured by the same admitted turn.
    #[must_use]
    pub fn with_channel_policy(mut self, policy: ChannelPolicySnapshot) -> Self {
        self.channel_policy = Some(policy);
        self
    }

    fn session(&self) -> Result<SessionId, ToolError> {
        self.session
            .ok_or_else(|| ToolError::Other("lifecycle tool requires a session".to_string()))
    }

    fn tx(&self) -> Result<&mpsc::UnboundedSender<LifecycleRequest>, ToolError> {
        self.tx.as_ref().ok_or_else(|| {
            ToolError::Other("lifecycle actions are unavailable outside a running team".to_string())
        })
    }

    /// Submit a terminal report (ADR-0015).
    ///
    /// # Errors
    /// Gate rejections surface as [`ToolError::Input`].
    pub async fn report(&self, outcome: ReportOutcome, report: String) -> Result<(), ToolError> {
        let session = self.session()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx()?
            .send(LifecycleRequest::Report {
                session,
                outcome,
                report,
                channel_policy: self.channel_policy,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::Other("lifecycle service unavailable".to_string()))?;
        flatten(reply_rx).await
    }

    /// Force-kill a direct child by handle.
    ///
    /// # Errors
    /// Rejections surface as [`ToolError::Input`].
    pub async fn kill(&self, handle: String, reason: String) -> Result<(), ToolError> {
        let session = self.session()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx()?
            .send(LifecycleRequest::Kill {
                session,
                handle,
                reason,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::Other("lifecycle service unavailable".to_string()))?;
        flatten(reply_rx).await
    }
}

async fn flatten(rx: oneshot::Receiver<Result<(), String>>) -> Result<(), ToolError> {
    match rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(ToolError::Input(message)),
        Err(_) => Err(ToolError::Other("lifecycle service dropped".to_string())),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn report_round_trips_through_the_plane() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let task = tokio::spawn(async move {
            plane
                .report(ReportOutcome::Done, "shipped".to_string())
                .await
        });
        match rx.recv().await.expect("request") {
            LifecycleRequest::Report {
                outcome,
                report,
                reply,
                ..
            } => {
                assert_eq!(outcome, ReportOutcome::Done);
                assert_eq!(report, "shipped");
                reply.send(Ok(())).expect("reply");
            }
            other => panic!("expected report, got {other:?}"),
        }
        task.await.unwrap().expect("accepted");
    }

    #[tokio::test]
    async fn gate_rejections_surface_as_input_errors() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let task =
            tokio::spawn(async move { plane.report(ReportOutcome::Done, "x".to_string()).await });
        if let LifecycleRequest::Report { reply, .. } = rx.recv().await.expect("request") {
            reply
                .send(Err("report rejected: unread mail".to_string()))
                .unwrap();
        }
        let error = task.await.unwrap().unwrap_err();
        assert!(matches!(error, ToolError::Input(message) if message.contains("unread")));
    }

    #[tokio::test]
    async fn disconnected_plane_fails_fast() {
        let error = LifecyclePlane::disconnected()
            .report(ReportOutcome::Done, "x".to_string())
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Other(_)));
    }

    #[tokio::test]
    async fn kill_requires_a_session_and_handle() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let task = tokio::spawn(async move {
            plane
                .kill("main/x-1".to_string(), "stuck".to_string())
                .await
        });
        match rx.recv().await.expect("request") {
            LifecycleRequest::Kill {
                handle,
                reason,
                reply,
                ..
            } => {
                assert_eq!(handle, "main/x-1");
                assert_eq!(reason, "stuck");
                reply.send(Ok(())).expect("reply");
            }
            other => panic!("expected kill, got {other:?}"),
        }
        task.await.unwrap().expect("killed");

        let unscoped = LifecyclePlane::new().0;
        assert!(matches!(
            unscoped.kill("x".to_string(), String::new()).await,
            Err(ToolError::Other(_))
        ));
    }
}
