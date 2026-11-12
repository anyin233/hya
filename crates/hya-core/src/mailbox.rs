//! The mailbox service loop: drains `MailboxRequest`s from the tool plane and
//! services them against the engine (ADR-0001).
//!
//! Mirrors `spawn_team_supervisor` usage of the spawner channel: the
//! app builds a `MailboxPlane` + receiver, injects the plane into the engine
//! (`with_mailbox`), and spawns [`run_mailbox_service`] to own the receiver. Each
//! request is handled by an engine method that appends the relevant event to the
//! team-root log and/or reads the team projection.

use std::sync::Arc;

use hya_tool::MailboxRequest;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::engine::SessionEngine;

/// Run the mailbox service until cancellation or the request channel closes; joins all handlers.
///
/// Typed engine errors are flattened to strings on the reply channel so the tool
/// plane — which lives in `hya-tool` and cannot see `CoreError` — can surface a
/// clean message. A dropped reply receiver (caller gone) is ignored.
pub async fn run_mailbox_service(
    engine: Arc<SessionEngine>,
    mut rx: UnboundedReceiver<MailboxRequest>,
    stop: tokio_util::sync::CancellationToken,
) {
    let mut requests = tokio::task::JoinSet::new();
    loop {
        let req = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            joined = requests.join_next(), if !requests.is_empty() => {
                if let Some(Err(error)) = joined {
                    tracing::error!(%error, "mailbox request task failed");
                }
                continue;
            }
            request = rx.recv() => match request {
                Some(request) => request,
                None => break,
            },
        };
        let engine = engine.clone();
        // Handle each request on its own task so one slow store read cannot head-of-
        // line block the others (sends/reads are independent per session).
        requests.spawn(async move {
            match req {
                MailboxRequest::ListChannels { session, reply } => {
                    let result = engine
                        .team_channel_rows(session)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::ReadChannel {
                    session,
                    channel,
                    last,
                    reply,
                } => {
                    let result = engine
                        .read_channel_history(session, &channel, last)
                        .await
                        .map(|read| {
                            (
                                read.channel,
                                read.messages,
                                read.unread_remaining,
                                read.warning,
                            )
                        })
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::TeamStatus { session, reply } => {
                    let result = engine
                        .team_member_status(session)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::SearchAgents {
                    session,
                    query,
                    reply,
                } => {
                    let result = engine
                        .search_archived_agents(session, &query)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                MailboxRequest::Send {
                    from,
                    actor_claim,
                    channel_policy,
                    to,
                    kind,
                    body,
                    reply,
                } => {
                    let result = if channel_policy.is_none() {
                        Err("channel policy snapshot missing for admitted send".to_string())
                    } else {
                        engine
                            .mail_send_for_actor_with_policy(
                                from,
                                to,
                                kind,
                                body,
                                actor_claim.as_ref(),
                                channel_policy,
                            )
                            .await
                            .map_err(|e| e.to_string())
                    };
                    let _ = reply.send(result);
                }
                MailboxRequest::SendDefault {
                    from,
                    actor_claim,
                    channel_policy,
                    body,
                    reply,
                } => {
                    let result = if channel_policy.is_none() {
                        Err("channel policy snapshot missing for admitted send".to_string())
                    } else {
                        engine
                            .mail_send_default_for_actor_with_policy(
                                from,
                                body,
                                actor_claim.as_ref(),
                                channel_policy,
                            )
                            .await
                            .map_err(|e| e.to_string())
                    };
                    let _ = reply.send(result);
                }
            }
        });
    }
    requests.abort_all();
    while let Some(joined) = requests.join_next().await {
        if let Err(error) = joined
            && !error.is_cancelled()
        {
            tracing::error!(%error, "request task failed during shutdown");
        }
    }
}
