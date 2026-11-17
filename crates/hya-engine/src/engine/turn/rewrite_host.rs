//! Full mode's side of the rewrite contract (`hya_tool::rewrite`): the
//! services the rewriter (the `hya/compaction` bundle) calls back into.
//!
//! The contract carries host failures as `ToolError`. Each service here also
//! records the original [`CoreError`] in [`HostErrors`], so a failed rewrite
//! ends the turn with that error (a stale actor claim, a missing fold agent)
//! rather than a flattened one.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hya_proto::{Event, Message, ModelRef, Projection, SessionId, UsagePurpose};
use hya_provider::ReasoningEffort;
use hya_store::ActorClaim;
use hya_tool::{
    CompactionHooks, FoldAgent, ModelHost, ModelWindow, SessionLog, SummaryCall, ToolError,
    Transcript,
};

use super::super::summary::summary_messages;
use super::super::{AgentSpec, FixedSystemAgent, SessionEngine, fixed_system_agent};
use super::messages::{AttachmentData, projection_to_messages};
use crate::TurnBinding;
use crate::compaction::{CompactionRung, SummarizeOptions, UsageCollector};
use crate::error::CoreError;
use crate::hooks::{
    CompactionAfterInput, CompactionBeforeInput, CompactionResolution, CompactionTrigger,
    resolve_compaction_decision,
};

impl SessionEngine {
    /// The settings `RewriteCtx::config` carries to the compaction bundle:
    /// thresholds, ladder order (rung wire names), and token accounting. A
    /// non-finite fraction travels as `null`, which the bundle treats like
    /// any fraction outside `(0, 1]`.
    pub(in crate::engine) fn rewriter_config(&self) -> serde_json::Value {
        let config = &self.compaction;
        serde_json::json!({
            "token_threshold": config.token_threshold,
            "keep_recent": config.keep_recent,
            "context_fraction": config.context_fraction,
            "reserve_tokens": config.reserve_tokens,
            "summary_max_tokens": config.summary_max_tokens,
            "method_order": config.method_order.map(CompactionRung::wire_name),
            "token_accounting": self.token_accounting.mode().as_str(),
        })
    }

    /// The window of `model`'s route, when it advertises one.
    pub(in crate::engine) fn model_window(&self, model: &ModelRef) -> Option<ModelWindow> {
        let router = self.provider_router();
        router.capabilities(model).map(|caps| ModelWindow {
            max_context: caps.max_context,
            usage_reporting: caps.usage_reporting,
            reasoning_replay: router.reasoning_replay_policy(model),
        })
    }
}

/// The last host error one rewrite saw, in its original type.
#[derive(Default)]
pub(in crate::engine) struct HostErrors(Mutex<Option<CoreError>>);

impl HostErrors {
    /// Keep `error` and hand the rewriter its `ToolError` form.
    fn record(&self, error: CoreError) -> ToolError {
        let tool = ToolError::Other(error.to_string());
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        tool
    }

    /// The error a failed rewrite ends with: the host error behind `error`,
    /// when the host raised one.
    pub(in crate::engine) fn into_error(self, error: ToolError) -> CoreError {
        self.0
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or_else(|| error.into())
    }
}

/// The session log a rewrite appends to, under the turn's actor claim.
pub(in crate::engine) struct TurnLog<'a> {
    pub(in crate::engine) engine: &'a SessionEngine,
    pub(in crate::engine) actor_claim: Option<&'a ActorClaim>,
    pub(in crate::engine) errors: &'a HostErrors,
}

#[async_trait]
impl SessionLog for TurnLog<'_> {
    async fn projection(&self, session: SessionId) -> Result<Arc<Projection>, ToolError> {
        self.engine
            .store
            .read_projection_shared(session)
            .await
            .map_err(|error| self.errors.record(error.into()))
    }

    async fn append(&self, session: SessionId, events: Vec<Event>) -> Result<(), ToolError> {
        self.engine
            .emit_batch_for_actor(self.actor_claim, session, events)
            .await
            .map_err(|error| self.errors.record(error))
    }
}

/// The two compaction hook points of the session's active hook chain. The
/// engine only rewrites on overflow, so a hook's skip is demoted and only a
/// replacement of the summarizer instructions carries through.
#[async_trait]
impl CompactionHooks for SessionEngine {
    async fn before(&self, session: SessionId, tokens: usize) -> Option<String> {
        let hooks = self.active_hook_dispatcher(session)?;
        let decision = hooks
            .compaction_before(CompactionBeforeInput {
                session,
                trigger: CompactionTrigger::Overflow,
                messages_token_estimate: tokens,
            })
            .await;
        match resolve_compaction_decision(decision, CompactionTrigger::Overflow) {
            CompactionResolution::Replace { instructions } => Some(instructions),
            CompactionResolution::Proceed | CompactionResolution::Skip { .. } => None,
        }
    }

    async fn after(&self, session: SessionId, summary_tokens: usize) {
        if let Some(hooks) = self.active_hook_dispatcher(session) {
            hooks
                .compaction_after(CompactionAfterInput {
                    session,
                    summary_tokens,
                })
                .await;
        }
    }
}

/// A rewrite's side model calls: the fixed agent that folds context, the
/// engine's summarizer, and the route's native compaction, billed to the
/// turn's actor.
pub(in crate::engine) struct TurnModels<'a> {
    pub(in crate::engine) engine: &'a SessionEngine,
    pub(in crate::engine) actor_claim: Option<&'a ActorClaim>,
    pub(in crate::engine) binding: &'a TurnBinding,
    /// Compaction before a round, Summary for `/summarize`.
    pub(in crate::engine) fold: FixedSystemAgent,
    pub(in crate::engine) errors: &'a HostErrors,
}

#[async_trait]
impl ModelHost for TurnModels<'_> {
    fn fold_agent(&self) -> Result<FoldAgent, ToolError> {
        let definition = fixed_system_agent(self.binding, self.fold)
            .map_err(|error| self.errors.record(error))?;
        let router = self.engine.provider_router();
        let options = super::super::summarize_options_from_definition(
            &definition,
            &self.engine.model_categories,
            self.binding.agent_model_preference(definition.stable_id),
            &|model| router.resolve(model).is_some(),
        );
        Ok(FoldAgent {
            model: options.model,
            system: options.system,
            reasoning: options.reasoning.map(|effort| effort.as_str().to_string()),
        })
    }

    fn has_summarizer(&self) -> bool {
        self.engine.summarizer.is_some()
    }

    async fn summarize(
        &self,
        session: SessionId,
        messages: &[Message],
        call: SummaryCall,
    ) -> Result<String, ToolError> {
        let summarizer = self.engine.summarizer.as_ref().ok_or_else(|| {
            self.errors
                .record(CoreError::Invalid("summarizer not configured".to_string()))
        })?;
        let usage = UsageCollector::default();
        let options = SummarizeOptions {
            system: call.system,
            model: call.model,
            reasoning: call.reasoning.as_deref().and_then(ReasoningEffort::parse),
            previous_summary: call.previous_summary,
            max_output_tokens: call.max_output_tokens,
            handoff: call.handoff,
            state_only: false,
            usage: Some(usage.clone()),
        };
        let summary = summarizer.summarize(messages, options).await;
        // Billed even when the call failed.
        self.engine
            .record_side_call_usage(self.actor_claim, session, UsagePurpose::Compaction, &usage)
            .await;
        summary.map_err(|error| self.errors.record(error))
    }

    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<Option<String>, ToolError> {
        let window = self
            .engine
            .provider_router()
            .compact_if_supported(model, messages, system)
            .await
            .map_err(|error| self.errors.record(error.into()))?;
        Ok(window.map(|window| hya_provider::format_responses_compact_system(&window.items)))
    }
}

/// The messages a full-mode round sends: the agent's rendering of the
/// projection. A rewrite only removes context, so the round's prompt images
/// cover every projection the rewrite produces.
pub(in crate::engine) struct TurnTranscript<'a> {
    pub(in crate::engine) agent: &'a AgentSpec,
    pub(in crate::engine) model: &'a ModelRef,
    pub(in crate::engine) attachments: &'a AttachmentData,
}

#[async_trait]
impl Transcript for TurnTranscript<'_> {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        Ok(projection_to_messages(
            self.agent,
            projection,
            self.model,
            self.attachments,
        ))
    }
}

/// What `/summarize` folds: the session's current episode.
pub(in crate::engine) struct EpisodeTranscript;

#[async_trait]
impl Transcript for EpisodeTranscript {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        summary_messages(projection).map_err(|error| ToolError::Other(error.to_string()))
    }
}
