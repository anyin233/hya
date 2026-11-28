//! The extension steps before each round, `shake` then `remote`, and the
//! `/summarize` fold (`remote`). Every change either makes to what a request
//! sends is an event in the session log, so the host rebuilds the request from
//! the projection. The kernel decides whether to reduce
//! (`RewriteCtx::pressure`) and rewrites itself after these steps when the
//! request is still over its threshold.

use std::sync::Arc;

use hya_context::rewrite::{RewriteCtx, ToolError};
use hya_context::tokens::{TokenAccounting, TokenSource};
use hya_proto::context::{ReasoningReplayPolicy, marker_criteria};
use hya_proto::{
    CompactionStrategy, Event, FinishReason, Message, MessageId, PartId, Role, SessionId,
};
use hya_runtime_abi::HostRuntime;

use crate::config::Config;
use crate::events::{CompactionEvent, ContextEvicted, ContextStatusProjection};
use crate::evict::{ArtifactEvictionSink, evict_stale_tool_outputs, has_spillable_tool_output};

async fn try_compact_window(
    cx: &RewriteCtx<'_>,
    runtime: &Arc<dyn HostRuntime>,
    messages: &[Message],
) -> Option<String> {
    match cx.models {
        Some(models) => models.compact_window(cx.model, messages).await.ok(),
        None => runtime
            .model_caller()
            .compact_window(cx.model, messages)
            .await
            .ok(),
    }
    .flatten()
}

/// Over the threshold, run `shake`, then `remote` while the request is still
/// over it; a route without native compaction, or a failed call, leaves the
/// rest to the kernel. Every round, record the round's `ContextStatus`.
/// `true` when the log changed.
pub(crate) async fn walk(
    cx: &RewriteCtx<'_>,
    runtime: &Arc<dyn HostRuntime>,
    config: Config,
) -> Result<bool, ToolError> {
    let accounting = TokenAccounting::new(cx.token_accounting)
        .with_tool_images(cx.window.is_some_and(|window| window.sends_tool_images));
    let session = cx.session;
    let mut messages = cx.transcript.messages(cx.projection).await?;
    // The in-flight assistant message does not count against the kept tail.
    let keep_messages = if matches!(messages.last(), Some(Message::Assistant { .. })) {
        config.keep_recent.saturating_add(1)
    } else {
        config.keep_recent
    };
    let keep_steps = config.keep_recent;
    let usage_reporting = cx.window.is_some_and(|window| window.usage_reporting);
    // Route-specific accounting must mirror what its encoder replays.
    let policy = cx.window.map_or(ReasoningReplayPolicy::None, |window| {
        window.reasoning_replay
    });
    let overhead = cx.overhead_tokens;
    let count = |messages: &[Message]| accounting.estimate_with_reasoning_policy(messages, policy);
    let initial =
        accounting.tokens_in_use_with_reasoning_policy(&messages, usage_reporting, policy);
    // The occupancy `ContextStatus` reports, in the configured accounting.
    let mut tokens = initial.tokens.saturating_add(overhead);
    let mut source = initial.source;
    let threshold = cx.pressure.map_or(0, |pressure| pressure.threshold);
    // The kernel's figure decides; each step lowers it by what it freed.
    let mut over = cx
        .pressure
        .filter(|pressure| pressure.over())
        .map(|pressure| pressure.used);
    let mut changed = false;
    if let Some(used) = over
        && has_spillable_tool_output(&messages, keep_messages, keep_steps)
    {
        let before = count(&messages);
        let evicted = evict_stale_tool_outputs(
            &mut messages,
            keep_messages,
            keep_steps,
            Some(&ArtifactEvictionSink::new(cx.workdir)),
        );
        if !evicted.is_empty() {
            let freed = before.saturating_sub(count(&messages));
            tokens = tokens.saturating_sub(freed);
            let after = used.saturating_sub(u64::try_from(freed).unwrap_or(u64::MAX));
            // The notices ride on the Event::Extension record, so later
            // requests send exactly these bytes.
            cx.log
                .append(
                    session,
                    vec![
                        CompactionEvent::ContextEvicted(ContextEvicted {
                            evicted_parts: u32::try_from(evicted.len()).unwrap_or(u32::MAX),
                            parts: evicted,
                            tokens_before: used,
                            tokens_after: after,
                            threshold,
                        })
                        .into_event(session),
                    ],
                )
                .await?;
            changed = true;
            over = Some(after).filter(|after| *after > threshold);
        }
    }
    if let Some(used) = over
        && let Some(body) = try_compact_window(cx, runtime, &messages).await
    {
        let summary_tokens = body.len() / 4;
        commit(cx, body, &messages, used, threshold).await?;
        changed = true;
        // Re-read and re-estimate: the provider's reported usage described
        // the prompt that no longer exists.
        let projection = cx.log.projection(session).await?;
        messages = cx.transcript.messages(&projection).await?;
        tokens = count(&messages).saturating_add(overhead);
        source = TokenSource::Estimate;
        if let Some(hooks) = cx.hooks {
            hooks.after(session, summary_tokens).await;
        }
    }
    // One occupancy report per round, after these steps, so the figure is
    // what this request carries unless the kernel rewrites after them.
    cx.log
        .append(
            session,
            vec![
                CompactionEvent::ContextStatus(ContextStatusProjection {
                    tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
                    source,
                    mode: accounting.mode(),
                    threshold,
                })
                .into_event(session),
            ],
        )
        .await?;
    Ok(changed)
}

/// Fold the transcript `cx` yields with the route's native compaction now
/// (`/summarize`); `false` when the route has none or the call failed, which
/// leaves the fold to the kernel.
pub(crate) async fn compact_now(
    cx: &RewriteCtx<'_>,
    runtime: &Arc<dyn HostRuntime>,
) -> Result<bool, ToolError> {
    let messages = cx.transcript.messages(cx.projection).await?;
    let Some(body) = try_compact_window(cx, runtime, &messages).await else {
        return Ok(false);
    };
    let (used, threshold) = cx
        .pressure
        .map_or((0, 0), |pressure| (pressure.used, pressure.threshold));
    commit(cx, body, &messages, used, threshold).await?;
    Ok(true)
}

/// Append a marker with `body` and the latest todo list, and the
/// `ContextCompacted{strategy: Native}` that folds all of `messages` behind
/// it, as one batch.
async fn commit(
    cx: &RewriteCtx<'_>,
    mut body: String,
    messages: &[Message],
    input_tokens_est: u64,
    threshold: u64,
) -> Result<(), ToolError> {
    let session = cx.session;
    let todos = cx.projection.session.todos.as_deref().unwrap_or_default();
    body.push_str("\n\n");
    body.push_str(&marker_criteria(todos));
    let message = MessageId::new();
    let mut events = marker_events(session, message, body);
    if let (Some(first), Some(last)) = (messages.first(), messages.last()) {
        events.push(Event::ContextCompacted {
            session,
            message,
            strategy: CompactionStrategy::Native,
            from_message: first.id(),
            to_message: last.id(),
            folded_count: u32::try_from(messages.len()).unwrap_or(u32::MAX),
            input_tokens_est,
            threshold,
            step_cut: None,
        });
    }
    cx.log.append(session, events).await
}

fn marker_events(session: SessionId, message: MessageId, text: String) -> Vec<Event> {
    let part = PartId::new();
    vec![
        Event::MessageStarted {
            session,
            message,
            role: Role::System,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextDelta {
            session,
            message,
            part,
            delta: text,
        },
        Event::TextEnd {
            session,
            message,
            part,
        },
        Event::MessageFinished {
            session,
            message,
            role: Role::System,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ]
}
