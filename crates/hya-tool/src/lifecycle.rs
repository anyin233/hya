//! Subagent lifecycle plane (ADR-0015): the `report` and `kill` tools ride a
//! narrow request channel to the resident supervisor, mirroring the mailbox
//! plane's dependency inversion (`hya-tool` never sees `CoreError`).

use hya_proto::{ReportOutcome, SessionId};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::mailbox::ChannelPolicySnapshot;
use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

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

/// Terminal report tool (ADR-0015): ends the calling agent's episode.
pub struct ReportTool;

#[async_trait::async_trait]
impl Tool for ReportTool {
    fn name(&self) -> &str {
        "report"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        obj_schema(
            "report",
            "Deliver your terminal report and end your episode. The engine checks you have no unread mail and no live children (answer or archive them first), writes your state handoff, delivers this report to your parent, and archives you. A follow-up from your parent can revive you with that handoff context.",
            json!({
                "result": {
                    "type": "string",
                    "description": "The result summary your parent receives"
                },
                "outcome": {
                    "type": "string",
                    "enum": ["done", "failed"],
                    "description": "Whether the task succeeded (default done)"
                }
            }),
            &["result"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let result = input
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| ToolError::Input("report requires a non-empty result".to_string()))?;
        let outcome = match input.get("outcome").and_then(Value::as_str) {
            Some("failed") => ReportOutcome::Failed,
            _ => ReportOutcome::Done,
        };
        ctx.lifecycle.report(outcome, result).await?;
        Ok(json!({
            "title": "Report accepted",
            "output": "Report accepted. Your episode ends when this turn completes; you will be archived with a state handoff. Your parent can revive you later.",
        }))
    }
}

/// Parent-side force kill (ADR-0015): archive a stuck direct child.
pub struct KillTool;

#[async_trait::async_trait]
impl Tool for KillTool {
    fn name(&self) -> &str {
        "kill"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        obj_schema(
            "kill",
            "Force-archive one of your direct subagents that is stuck or blocks your own report. The child is cancelled, a degraded handoff is written, and you receive its synthesized failure report.",
            json!({
                "handle": {
                    "type": "string",
                    "description": "The child handle to kill (from task or search_agent)"
                },
                "reason": {
                    "type": "string",
                    "description": "Short reason; delivered to you as the failure report"
                }
            }),
            &["handle"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let handle = input
            .get("handle")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| ToolError::Input("kill requires a handle".to_string()))?
            .to_string();
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "killed by parent".to_string());
        ctx.lifecycle.kill(handle, reason).await?;
        Ok(json!({
            "title": "Killed",
            "output": "The child was archived with a synthesized failure report and a degraded handoff.",
        }))
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
