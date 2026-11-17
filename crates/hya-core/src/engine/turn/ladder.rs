//! The full mode's [`Rewriter`]: the compaction ladder walked before each
//! round, over a transcript with this session's sticky evictions applied.

use hya_proto::{
    CompactionStrategy, Event, Message, MessageId, ModelRef, Projection, SessionId, UsagePurpose,
};
use hya_store::ActorClaim;

use super::super::spill::ArtifactEvictionSink;
use super::super::{
    AgentSpec, FixedSystemAgent, SessionEngine, fixed_system_agent, session_workdir,
};
use super::messages::{projection_to_messages, system_prompt};
use crate::TurnBinding;
use crate::agent_catalog::AgentDefinition;
use crate::error::CoreError;
use crate::hooks::{
    CompactionAfterInput, CompactionBeforeInput, CompactionResolution, CompactionTrigger,
    resolve_compaction_decision,
};
use crate::kernel::Rewriter;
use crate::runtime_registry::CompiledResourceView;

/// Range endpoints for a compaction that folded the entire input window.
///
/// Native provider compact is handed the whole window, unlike the local
/// summarizer which folds only the prefix before the retained recent messages.
/// `None` for an empty window, which cannot trip the threshold anyway.
fn whole_window_range(messages: &[Message]) -> Option<(MessageId, MessageId, u32)> {
    let first = messages.first()?;
    let last = messages.last()?;
    Some((
        first.id(),
        last.id(),
        u32::try_from(messages.len()).unwrap_or(u32::MAX),
    ))
}

/// Override the summarizer system prompt when a `compaction.before` hook
/// returned `Replace` instructions for this walk; otherwise keep the options
/// the definition and transcript imply.
fn with_hook_instructions(
    mut options: crate::compaction::SummarizeOptions,
    instructions: &Option<String>,
) -> crate::compaction::SummarizeOptions {
    if let Some(text) = instructions {
        options.system = Some(text.clone());
    }
    options
}

/// One round's compaction ladder of a full-mode turn.
pub(super) struct CompactionLadder<'a> {
    pub(super) engine: &'a SessionEngine,
    pub(super) session: SessionId,
    pub(super) agent: &'a AgentSpec,
    pub(super) model: &'a ModelRef,
    pub(super) binding: &'a TurnBinding,
    pub(super) resources: &'a CompiledResourceView,
    pub(super) depth: u32,
    pub(super) actor_claim: Option<&'a ActorClaim>,
    /// The transcript the round's request sends, set by `before_round`.
    pub(super) messages: Vec<Message>,
}

#[async_trait::async_trait]
impl Rewriter for CompactionLadder<'_> {
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError> {
        let &mut Self {
            engine,
            session,
            agent: live_agent,
            model,
            binding,
            resources,
            depth,
            actor_claim,
            ..
        } = self;
        let attachments = engine.attachment_data(session, projection).await?;
        let mut messages = projection_to_messages(live_agent, projection, model, &attachments);
        let mut fold_config = engine.compaction;
        if matches!(messages.last(), Some(Message::Assistant { .. })) {
            fold_config.keep_recent = fold_config.keep_recent.saturating_add(1);
        }
        // Earlier evictions of this session stay in effect: re-apply their
        // exact placeholders before measuring, so the prefix stays
        // byte-stable (provider prompt cache) and a request they already
        // keep under the threshold does not walk the ladder again.
        let mut sticky_evictions = engine
            .sticky_evictions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session)
            .unwrap_or_default();
        crate::compaction::apply_sticky_evictions(&mut messages, &sticky_evictions);

        // Active route for this turn. Its advertised context window scales
        // the compaction threshold, so resolve it before deciding.
        let capabilities = engine.provider_router().capabilities(model);
        let resolved_threshold = crate::compaction::resolved_threshold(
            &engine.compaction,
            capabilities.as_ref().map(|c| c.max_context),
        );
        // One running token count for the whole reduction sequence. It starts
        // with the request occupancy and is adjusted after each rung so the
        // next decision reflects the transcript that will actually be sent.
        // Route-specific accounting must mirror what its encoder replays.
        let usage_reporting = capabilities.as_ref().is_some_and(|c| c.usage_reporting);
        let reasoning_policy = engine.provider_router().reasoning_replay_policy(model);
        let initial_count = engine.token_accounting.tokens_in_use_with_reasoning_policy(
            &messages,
            usage_reporting,
            reasoning_policy,
        );
        let tools = engine.offered_tools(resources, depth);
        let template = crate::kernel::request::from_messages(
            Vec::new(),
            &system_prompt(live_agent, &tools, depth),
            tools,
            model,
            None,
        );
        let request_overhead = engine
            .token_accounting
            .estimate_request(&template, reasoning_policy);
        let count = |messages: &[Message]| {
            engine
                .token_accounting
                .estimate_with_reasoning_policy(messages, reasoning_policy)
        };
        let max_context = capabilities.as_ref().map_or(0, |c| c.max_context as usize);
        let over_window = |messages: &[Message]| {
            max_context > 0 && count(messages).saturating_add(request_overhead) > max_context
        };
        // When the local preflight (`check_request_context`) would reject
        // the request outright even though provider-reported occupancy from
        // the previous step sits below `resolved_threshold`, measure by the
        // local estimate so the ladder reduces the request instead of
        // letting the preflight fail the turn un-compacted.
        let (mut tokens, mut token_source) = {
            let reported = initial_count.tokens.saturating_add(request_overhead);
            if reported <= resolved_threshold && over_window(&messages) {
                (
                    count(&messages).saturating_add(request_overhead),
                    crate::tokens::TokenSource::Estimate,
                )
            } else {
                (reported, initial_count.source)
            }
        };
        let needs_reduction = |tokens: usize, messages: &[Message]| {
            tokens > resolved_threshold || over_window(messages)
        };
        let fold_cut = |messages: &[Message]| {
            crate::compaction::fold_cut(
                messages,
                fold_config.keep_recent,
                engine.compaction.keep_recent,
                resolved_threshold,
                &count,
            )
        };

        // Reduction ladder: the five built-in mechanisms (oh-my-pi parity),
        // walked in the configured order — `compaction.method_order`. The
        // walk stops at the first rung that brings the transcript under the
        // threshold, so a turn never pays for a model call that spilling
        // alone would have avoided, and an unavailable or failed rung
        // (unsupported route, no summarizer wired) advances to the next.
        // Escalation matters in the other direction too: a native compact
        // that succeeded but left the transcript over threshold used to end
        // the sequence, sending the request out still over the window it
        // was trying to fit.
        let spill = ArtifactEvictionSink::new(&session_workdir(live_agent, projection));
        // Resolved on first use, so a turn that an earlier rung rescued
        // never requires the fixed Compaction agent to exist.
        let mut compaction_agent: Option<AgentDefinition<'_>> = None;

        // Consult `compaction.before` once per ladder walk, only when the
        // walk will actually run (over threshold). The engine's only
        // trigger is Overflow: the request cannot go out un-compacted, so
        // `resolve_compaction_decision` demotes any Skip to a warning and
        // a Replace only rewrites the summarizer instructions below.
        let summarizer_instructions = if needs_reduction(tokens, &messages) {
            match engine.active_hook_dispatcher(session) {
                Some(hooks) => {
                    let decision = hooks
                        .compaction_before(CompactionBeforeInput {
                            session,
                            trigger: CompactionTrigger::Overflow,
                            messages_token_estimate: tokens,
                        })
                        .await;
                    match resolve_compaction_decision(decision, CompactionTrigger::Overflow) {
                        CompactionResolution::Replace { instructions } => Some(instructions),
                        // Overflow-triggered skips are demoted by the
                        // resolver; nothing to carry into the ladder.
                        CompactionResolution::Proceed | CompactionResolution::Skip { .. } => None,
                    }
                }
                None => None,
            }
        } else {
            None
        };
        // Estimated size of the last summary this walk committed, reported
        // to `compaction.after` once the ladder is done.
        let mut committed_summary_tokens: Option<usize> = None;
        let mut rewrote = false;

        for rung in engine.compaction.method_order {
            if !needs_reduction(tokens, &messages) {
                break;
            }
            if matches!(rung, crate::compaction::CompactionRung::SpillToolOutputs)
                && !crate::compaction::has_spillable_tool_output(
                    &messages,
                    fold_config.keep_recent,
                    engine.compaction.keep_recent,
                )
            {
                continue;
            }
            // Snapshot what tripped the threshold before this rung edits the
            // transcript, so each record explains why that rung ran.
            let estimated_now = count(&messages).saturating_add(request_overhead);
            let input_tokens_est = u64::try_from(if over_window(&messages) {
                tokens.max(estimated_now)
            } else {
                tokens
            })
            .unwrap_or(u64::MAX);
            let threshold = u64::try_from(resolved_threshold).unwrap_or(u64::MAX);

            match rung {
                crate::compaction::CompactionRung::SpillToolOutputs => {
                    let estimate_before = engine
                        .token_accounting
                        .estimate_with_reasoning_policy(&messages, reasoning_policy);
                    let evicted = crate::compaction::evict_stale_tool_outputs_in_turn(
                        &mut messages,
                        fold_config.keep_recent,
                        engine.compaction.keep_recent,
                        Some(&spill),
                        &mut sticky_evictions,
                    );
                    if evicted == 0 {
                        continue;
                    }
                    let saved = estimate_before.saturating_sub(
                        engine
                            .token_accounting
                            .estimate_with_reasoning_policy(&messages, reasoning_policy),
                    );
                    tokens = tokens.saturating_sub(saved);
                    // Record the saving whether or not it sufficed. A partial
                    // reduction that still needed a summary is real work, and
                    // hiding it made the ledger disagree with the transcript.
                    engine
                        .emit_for_actor(
                            actor_claim,
                            session,
                            Event::ContextEvicted {
                                session,
                                evicted_parts: evicted,
                                tokens_before: input_tokens_est,
                                tokens_after: u64::try_from(tokens).unwrap_or(u64::MAX),
                                threshold,
                            },
                        )
                        .await?;
                    rewrote = true;
                }
                crate::compaction::CompactionRung::ProviderCompact => {
                    // Exact-resolve the fixed Compaction agent before any
                    // compact provider call. Missing definition fails closed.
                    // Reuse the turn's captured binding; never re-bind or open
                    // a second catalog.
                    let definition = match &compaction_agent {
                        Some(definition) => definition,
                        None => compaction_agent
                            .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                    };
                    let options = engine.folding_options(definition, binding, &messages);
                    let compaction_model = options.model.clone().unwrap_or_else(|| model.clone());
                    let Ok(Some(window)) = engine
                        .provider_router()
                        .compact_if_supported(&compaction_model, &messages, definition.prompt)
                        .await
                    else {
                        continue;
                    };
                    let body = hya_provider::format_responses_compact_system(&window.items);
                    // Native compact folds the whole input window it was given.
                    let folded = whole_window_range(&messages);
                    committed_summary_tokens = Some(body.len() / 4);
                    // Persist so subsequent rounds re-inject the compact window
                    // and drop pre-marker history via HYA_COMPACTED_CONTEXT.
                    engine
                        .commit_compaction(
                            actor_claim,
                            session,
                            body,
                            CompactionStrategy::Native,
                            folded,
                            None,
                            input_tokens_est,
                            threshold,
                        )
                        .await?;
                    (messages, tokens) = engine
                        .reload_after_compaction(
                            session,
                            live_agent,
                            model,
                            &sticky_evictions,
                            reasoning_policy,
                        )
                        .await?;
                    tokens = tokens.saturating_add(request_overhead);
                    token_source = crate::tokens::TokenSource::Estimate;
                }
                crate::compaction::CompactionRung::SnapCompact => {
                    // Local and deterministic: no model call, no capability
                    // gate. Folds the same prefix a summary would into the
                    // dense archive, so the ladder's model-free rung works
                    // even where no summarizer is wired at all.
                    let Some(cut) = fold_cut(&messages) else {
                        continue;
                    };
                    let archive = crate::compaction::snapcompact_archive(&cut.folded);
                    let body = format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, archive);
                    committed_summary_tokens = Some(body.len() / 4);
                    engine
                        .commit_compaction(
                            actor_claim,
                            session,
                            body,
                            CompactionStrategy::SnapCompact,
                            Some((cut.from_message, cut.to_message, cut.folded_count)),
                            cut.step_cut,
                            input_tokens_est,
                            threshold,
                        )
                        .await?;
                    (messages, tokens) = engine
                        .reload_after_compaction(
                            session,
                            live_agent,
                            model,
                            &sticky_evictions,
                            reasoning_policy,
                        )
                        .await?;
                    tokens = tokens.saturating_add(request_overhead);
                    token_source = crate::tokens::TokenSource::Estimate;
                }
                crate::compaction::CompactionRung::Handoff => {
                    let Some(summarizer) = &engine.summarizer else {
                        continue;
                    };
                    let Some(cut) = fold_cut(&messages) else {
                        continue;
                    };
                    let definition = match &compaction_agent {
                        Some(definition) => definition,
                        None => compaction_agent
                            .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                    };
                    // The handoff call sees the whole transcript verbatim;
                    // the fold range still leaves the recent tail in place.
                    // A `compaction.before` Replace decision steers the
                    // summarizer prompt for this walk.
                    let usage = crate::compaction::UsageCollector::default();
                    let options = crate::compaction::SummarizeOptions {
                        usage: Some(usage.clone()),
                        ..with_hook_instructions(
                            engine.folding_options(definition, binding, &messages),
                            &summarizer_instructions,
                        )
                    };
                    let planned = crate::compaction::plan_handoff(
                        &messages,
                        cut,
                        summarizer.as_ref(),
                        options,
                    )
                    .await;
                    // Billed even when the rung then fails or is skipped.
                    engine
                        .record_side_call_usage(
                            actor_claim,
                            session,
                            UsagePurpose::Compaction,
                            &usage,
                        )
                        .await;
                    let Ok(plan) = planned else {
                        continue;
                    };
                    let body =
                        format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, plan.summary);
                    committed_summary_tokens = Some(body.len() / 4);
                    engine
                        .commit_compaction(
                            actor_claim,
                            session,
                            body,
                            CompactionStrategy::Handoff,
                            Some((plan.from_message, plan.to_message, plan.folded_count)),
                            plan.step_cut,
                            input_tokens_est,
                            threshold,
                        )
                        .await?;
                    (messages, tokens) = engine
                        .reload_after_compaction(
                            session,
                            live_agent,
                            model,
                            &sticky_evictions,
                            reasoning_policy,
                        )
                        .await?;
                    tokens = tokens.saturating_add(request_overhead);
                    token_source = crate::tokens::TokenSource::Estimate;
                }
                crate::compaction::CompactionRung::Summarize => {
                    let Some(summarizer) = &engine.summarizer else {
                        continue;
                    };
                    let Some(cut) = fold_cut(&messages) else {
                        continue;
                    };
                    let definition = match &compaction_agent {
                        Some(definition) => definition,
                        None => compaction_agent
                            .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                    };
                    // Provider failures stay soft; a missing definition has
                    // already failed closed by the time we are here. A
                    // `compaction.before` Replace decision steers the
                    // summarizer prompt for this walk.
                    let usage = crate::compaction::UsageCollector::default();
                    let options = crate::compaction::SummarizeOptions {
                        usage: Some(usage.clone()),
                        ..with_hook_instructions(
                            engine.folding_options(definition, binding, &messages),
                            &summarizer_instructions,
                        )
                    };
                    let planned =
                        crate::compaction::fold_prefix(cut, summarizer.as_ref(), options).await;
                    // Billed even when the rung then fails or is skipped.
                    engine
                        .record_side_call_usage(
                            actor_claim,
                            session,
                            UsagePurpose::Compaction,
                            &usage,
                        )
                        .await;
                    let Ok(plan) = planned else {
                        continue;
                    };
                    // Persist the local summary behind the same marker the
                    // native path uses. Without this the summary died with the
                    // request and every later round re-summarized the same
                    // history.
                    let body =
                        format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, plan.summary);
                    committed_summary_tokens = Some(body.len() / 4);
                    engine
                        .commit_compaction(
                            actor_claim,
                            session,
                            body,
                            CompactionStrategy::LocalSummarizer,
                            Some((plan.from_message, plan.to_message, plan.folded_count)),
                            plan.step_cut,
                            input_tokens_est,
                            threshold,
                        )
                        .await?;
                    (messages, tokens) = engine
                        .reload_after_compaction(
                            session,
                            live_agent,
                            model,
                            &sticky_evictions,
                            reasoning_policy,
                        )
                        .await?;
                    tokens = tokens.saturating_add(request_overhead);
                    token_source = crate::tokens::TokenSource::Estimate;
                }
            }
        }
        while sticky_evictions.len() > 4096 {
            if let Some(key) = sticky_evictions.keys().next().copied() {
                sticky_evictions.remove(&key);
            }
        }
        engine
            .sticky_evictions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, sticky_evictions);
        {
            let mut sticky = engine
                .sticky_evictions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if sticky.len() > 64
                && let Some(other) = sticky.keys().find(|key| **key != session).copied()
            {
                sticky.remove(&other);
            }
        }
        // Notify `compaction.after` best-effort: an enrichment point, so a
        // failure inside the host is logged there and never surfaced here.
        if let (Some(hooks), Some(summary_tokens)) = (
            engine.active_hook_dispatcher(session),
            committed_summary_tokens,
        ) {
            hooks
                .compaction_after(CompactionAfterInput {
                    session,
                    summary_tokens,
                })
                .await;
        }
        // The wire surface of token accounting: one report per round,
        // recorded after the ladder so the figure is the occupancy this
        // request actually carries.
        engine
            .emit_for_actor(
                actor_claim,
                session,
                Event::ContextStatus {
                    session,
                    tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
                    source: token_source,
                    mode: engine.token_accounting.mode(),
                    threshold: u64::try_from(resolved_threshold).unwrap_or(u64::MAX),
                },
            )
            .await?;
        self.messages = messages;
        Ok(rewrote || committed_summary_tokens.is_some())
    }
}
