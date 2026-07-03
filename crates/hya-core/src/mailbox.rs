//! The mailbox service loop: drains [`MailboxRequest`]s from the tool plane and
//! services them against the engine (ADR-0001).
//!
//! Mirrors [`spawn_team_supervisor`](crate::) usage of the spawner channel: the
//! app builds a [`MailboxPlane`] + receiver, injects the plane into the engine
//! (`with_mailbox`), and spawns [`run_mailbox_service`] to own the receiver. Each
//! request is handled by an engine method that appends the relevant event to the
//! team-root log and/or reads the team projection.

use std::sync::Arc;

use hya_tool::MailboxRequest;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::engine::SessionEngine;

/// Run the mailbox service until the plane (and all its clones) are dropped.
///
/// Typed engine errors are flattened to strings on the reply channel so the tool
/// plane — which lives in `hya-tool` and cannot see `CoreError` — can surface a
/// clean message. A dropped reply receiver (caller gone) is ignored.
pub async fn run_mailbox_service(
    engine: Arc<SessionEngine>,
    mut rx: UnboundedReceiver<MailboxRequest>,
) {
    while let Some(req) = rx.recv().await {
        let engine = engine.clone();
        // Handle each request on its own task so one slow store read cannot head-of-
        // line block the others (sends/reads are independent per session).
        tokio::spawn(async move {
            match req {
                MailboxRequest::Send {
                    from,
                    to,
                    kind,
                    body,
                    reply,
                } => {
                    let result = engine
                        .mail_send(from, to, kind, body)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::Join {
                    session,
                    channel,
                    reply,
                } => {
                    let result = engine
                        .channel_join(session, channel)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::Leave {
                    session,
                    channel,
                    reply,
                } => {
                    let result = engine
                        .channel_leave(session, channel)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::Roster { session, reply } => {
                    let result = engine.team_roster(session).await.map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::Channels { session, reply } => {
                    let result = engine
                        .team_channels(session)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
            }
        });
    }
}
