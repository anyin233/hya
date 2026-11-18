//! The reduction ladder before each round, and the `/summarize` fold. Every
//! change either makes to what a request sends is an event in the session log,
//! so the host rebuilds the request from the projection.

use hya_proto::context::{COMPACT_CONTEXT_MARKER, ReasoningReplayPolicy, render_todos};
use hya_proto::{
    CompactionStrategy, Event, FinishReason, Message, MessageId, PartId, Role, StepCut,
};
use hya_tool::tokens::{TokenAccounting, TokenSource};
use hya_tool::{ModelHost, RewriteCtx, SummaryCall, ToolError};

use crate::config::{Config, Rung};
use crate::evict::{ArtifactEvictionSink, evict_stale_tool_outputs, has_spillable_tool_output};
use crate::fold::{fold_cut, previous_summary};
use crate::snap::snapcompact_archive;

/// Walk the configured rungs while the transcript `cx` would send is over the
/// threshold, stopping at the first that brings it under; an unavailable or
/// failed rung advances to the next. Then record the round's `ContextStatus`.
/// `true` when the log changed.
pub(crate) async fn walk(cx: &RewriteCtx<'_>) -> Result<bool, ToolError> {
    let config = Config::from_value(cx.config)?;
    let accounting = TokenAccounting::new(config.token_accounting);
    let session = cx.session;
    let mut messages = cx.transcript.messages(cx.projection).await?;
    // The in-flight assistant message does not count against the kept tail.
    let keep_messages = if matches!(messages.last(), Some(Message::Assistant { .. })) {
        config.keep_recent.saturating_add(1)
    } else {
        config.keep_recent
    };
    let keep_steps = config.keep_recent;
    // The route's advertised window scales the threshold.
    let threshold = config.threshold(cx.window.map(|window| window.max_context));
    let usage_reporting = cx.window.is_some_and(|window| window.usage_reporting);
    // Route-specific accounting must mirror what its encoder replays.
    let policy = cx.window.map_or(ReasoningReplayPolicy::None, |window| {
        window.reasoning_replay
    });
    let max_context = cx.window.map_or(0, |window| {
        usize::try_from(window.max_context).unwrap_or(usize::MAX)
    });
    let overhead = cx.overhead_tokens;
    let count = |messages: &[Message]| accounting.estimate_with_reasoning_policy(messages, policy);
    let over_window = |messages: &[Message]| {
        max_context > 0 && count(messages).saturating_add(overhead) > max_context
    };
    // One running count for the whole walk, adjusted after each rung so the
    // next decision reflects the transcript that will actually be sent. When
    // the host's pre-send check would reject the request although the
    // reported occupancy sits under the threshold, measure by the estimate,
    // so the ladder reduces the request instead of the check failing the turn.
    let initial =
        accounting.tokens_in_use_with_reasoning_policy(&messages, usage_reporting, policy);
    let (mut tokens, mut source) = {
        let reported = initial.tokens.saturating_add(overhead);
        if reported <= threshold && over_window(&messages) {
            (
                count(&messages).saturating_add(overhead),
                TokenSource::Estimate,
            )
        } else {
            (reported, initial.source)
        }
    };
    let needs_reduction =
        |tokens: usize, messages: &[Message]| tokens > threshold || over_window(messages);
    let spill = ArtifactEvictionSink::new(cx.workdir);
    // `compaction.before` once per walk, only when the walk runs. The only
    // trigger is overflow, so a hook may only replace the summarizer prompt.
    let instructions = match cx.hooks {
        Some(hooks) if needs_reduction(tokens, &messages) => hooks.before(session, tokens).await,
        _ => None,
    };
    let estimate_tokens = |tokens: usize| u64::try_from(tokens).unwrap_or(u64::MAX);
    let threshold_tokens = estimate_tokens(threshold);
    // Estimated size of the last summary the walk committed, for
    // `compaction.after`.
    let mut summary_tokens: Option<usize> = None;
    let mut rewrote = false;

    for rung in config.method_order {
        if !needs_reduction(tokens, &messages) {
            break;
        }
        if rung == Rung::SpillToolOutputs
            && !has_spillable_tool_output(&messages, keep_messages, keep_steps)
        {
            continue;
        }
        // What tripped the threshold before this rung edits the transcript,
        // so each record explains why that rung ran.
        let estimated_now = count(&messages).saturating_add(overhead);
        let input_tokens_est = estimate_tokens(if over_window(&messages) {
            tokens.max(estimated_now)
        } else {
            tokens
        });
        let fold =
            |messages: &[Message]| fold_cut(messages, keep_messages, keep_steps, threshold, &count);
        let mut committed = match rung {
            Rung::SpillToolOutputs => {
                let before = count(&messages);
                let evicted = evict_stale_tool_outputs(
                    &mut messages,
                    keep_messages,
                    keep_steps,
                    Some(&spill),
                );
                if evicted.is_empty() {
                    continue;
                }
                tokens = tokens.saturating_sub(before.saturating_sub(count(&messages)));
                // Recorded whether or not it sufficed; the notices ride on the
                // event, so later requests send exactly these bytes.
                cx.log
                    .append(
                        session,
                        vec![Event::ContextEvicted {
                            session,
                            evicted_parts: u32::try_from(evicted.len()).unwrap_or(u32::MAX),
                            parts: evicted,
                            tokens_before: input_tokens_est,
                            tokens_after: estimate_tokens(tokens),
                            threshold: threshold_tokens,
                        }],
                    )
                    .await?;
                rewrote = true;
                continue;
            }
            Rung::ProviderCompact => {
                let Some(models) = cx.models else {
                    continue;
                };
                // The fold agent must exist before any compact call.
                let agent = models.fold_agent()?;
                let model = agent.model.unwrap_or_else(|| cx.model.clone());
                let Ok(Some(body)) = models
                    .compact_window(&model, &messages, agent.system.as_deref())
                    .await
                else {
                    continue;
                };
                // Native compact folds the whole window it was given.
                Fold {
                    body,
                    strategy: CompactionStrategy::Native,
                    folded: whole_range(&messages),
                    step_cut: None,
                }
            }
            Rung::SnapCompact => {
                // Local and deterministic: works where no summarizer is wired.
                let Some(cut) = fold(&messages) else {
                    continue;
                };
                Fold {
                    body: format!(
                        "{COMPACT_CONTEXT_MARKER}\n{}",
                        snapcompact_archive(&cut.folded)
                    ),
                    strategy: CompactionStrategy::SnapCompact,
                    folded: Some((cut.from_message, cut.to_message, cut.folded_count)),
                    step_cut: cut.step_cut,
                }
            }
            Rung::Handoff | Rung::Summarize => {
                let Some(models) = cx.models.filter(|models| models.has_summarizer()) else {
                    continue;
                };
                let Some(cut) = fold(&messages) else {
                    continue;
                };
                let handoff = rung == Rung::Handoff;
                let mut call = summary_call(models, &messages, &config)?;
                call.handoff = handoff;
                if let Some(text) = &instructions {
                    call.system = Some(text.clone());
                }
                // A handoff reads the whole transcript verbatim; the fold
                // range still keeps the recent tail. A failed call is soft.
                let input = if handoff { &messages } else { &cut.folded };
                let Ok(summary) = models.summarize(session, input, call).await else {
                    continue;
                };
                Fold {
                    body: format!("{COMPACT_CONTEXT_MARKER}\n{summary}"),
                    strategy: if handoff {
                        CompactionStrategy::Handoff
                    } else {
                        CompactionStrategy::LocalSummarizer
                    },
                    folded: Some((cut.from_message, cut.to_message, cut.folded_count)),
                    step_cut: cut.step_cut,
                }
            }
        };
        committed.carry_todos(cx);
        summary_tokens = Some(committed.body.len() / 4);
        commit(cx, committed, input_tokens_est, threshold_tokens).await?;
        // Re-read and re-estimate: the provider's reported usage described
        // the prompt that no longer exists.
        let projection = cx.log.projection(session).await?;
        messages = cx.transcript.messages(&projection).await?;
        tokens = count(&messages).saturating_add(overhead);
        source = TokenSource::Estimate;
    }
    if let (Some(hooks), Some(summary_tokens)) = (cx.hooks, summary_tokens) {
        hooks.after(session, summary_tokens).await;
    }
    // One occupancy report per round, after the ladder, so the figure is
    // what this request carries.
    cx.log
        .append(
            session,
            vec![Event::ContextStatus {
                session,
                tokens: estimate_tokens(tokens),
                source,
                mode: accounting.mode(),
                threshold: threshold_tokens,
            }],
        )
        .await?;
    Ok(rewrote || summary_tokens.is_some())
}

/// Fold the whole episode `cx.transcript` yields into one summary now
/// (`/summarize`).
pub(crate) async fn summarize_now(cx: &RewriteCtx<'_>) -> Result<bool, ToolError> {
    let config = Config::from_value(cx.config)?;
    let models = cx
        .models
        .ok_or_else(|| ToolError::Other("summarizer not configured".to_string()))?;
    let messages = cx.transcript.messages(cx.projection).await?;
    let mut call = summary_call(models, &messages, &config)?;
    // Anchor on the last terminal handoff when the episode carries no summary.
    call.previous_summary = call.previous_summary.or_else(|| {
        cx.projection
            .session
            .handoff
            .as_ref()
            .map(|handoff| handoff.doc.clone())
    });
    let summary = models.summarize(cx.session, &messages, call).await?;
    let tokens = TokenAccounting::new(config.token_accounting).estimate(&messages);
    // Behind the marker, so the request cuts the folded history.
    let mut fold = Fold {
        body: format!("{COMPACT_CONTEXT_MARKER}\n{summary}"),
        strategy: CompactionStrategy::LocalSummarizer,
        folded: whole_range(&messages),
        step_cut: None,
    };
    fold.carry_todos(cx);
    commit(cx, fold, u64::try_from(tokens).unwrap_or(u64::MAX), 0).await?;
    Ok(true)
}

/// The fold agent's summary call, anchored on the summary `messages` carry.
fn summary_call(
    models: &dyn ModelHost,
    messages: &[Message],
    config: &Config,
) -> Result<SummaryCall, ToolError> {
    let agent = models.fold_agent()?;
    Ok(SummaryCall {
        model: agent.model,
        system: agent.system,
        reasoning: agent.reasoning,
        previous_summary: previous_summary(messages),
        max_output_tokens: Some(config.summary_max_tokens),
        handoff: false,
    })
}

/// The range of a fold of all of `messages`; `None` when there are none.
fn whole_range(messages: &[Message]) -> Option<(MessageId, MessageId, u32)> {
    let (first, last) = messages.first().zip(messages.last())?;
    Some((
        first.id(),
        last.id(),
        u32::try_from(messages.len()).unwrap_or(u32::MAX),
    ))
}

/// One fold to commit: the marker body and the range it replaces.
struct Fold {
    body: String,
    strategy: CompactionStrategy,
    /// First and last folded message and how many; `None` records the marker
    /// alone.
    folded: Option<(MessageId, MessageId, u32)>,
    step_cut: Option<StepCut>,
}

impl Fold {
    fn carry_todos(&mut self, cx: &RewriteCtx<'_>) {
        if let Some(todos) = &cx.projection.session.todos
            && !todos.is_empty()
        {
            self.body.push_str("\n\n");
            self.body.push_str(&render_todos(todos));
        }
    }
}

/// Append the marker message and its `ContextCompacted` as one batch.
async fn commit(
    cx: &RewriteCtx<'_>,
    fold: Fold,
    input_tokens_est: u64,
    threshold: u64,
) -> Result<(), ToolError> {
    let session = cx.session;
    let message = MessageId::new();
    let part = PartId::new();
    let mut events = vec![
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
            delta: fold.body,
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
    ];
    if let Some((from_message, to_message, folded_count)) = fold.folded {
        events.push(Event::ContextCompacted {
            session,
            message,
            strategy: fold.strategy,
            from_message,
            to_message,
            folded_count,
            input_tokens_est,
            threshold,
            step_cut: fold.step_cut,
        });
    }
    cx.log.append(session, events).await
}
