//! Full mode's side of the rewrite contract (`hya_tool::rewrite`): what a
//! bundle rewriter calls back into. The session log is the kernel's
//! [`crate::kernel::Scope`] over the turn's actor-claimed emit.

use async_trait::async_trait;
use hya_proto::{Message, MessageId, ModelRef, Projection, SessionId};
use hya_provider::{CompletionRequest, ReasoningEffort};
use hya_store::ActorClaim;
use hya_tool::{CompactionHooks, FoldAgent, ModelCall, ModelHost, ToolError, Transcript};

use super::super::{AgentSpec, FixedSystemAgent, SessionEngine, fixed_system_agent};
use super::messages::projection_to_messages;
use crate::TurnBinding;
use crate::hooks::{
    CompactionAfterInput, CompactionBeforeInput, CompactionResolution, CompactionTrigger,
    resolve_compaction_decision,
};

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

/// A turn's side model calls: the fixed agent that folds its context, and
/// completions billed to the turn's actor.
#[expect(dead_code, reason = "constructed once the compaction bundle is wired")]
pub(super) struct TurnModels<'a> {
    pub(super) engine: &'a SessionEngine,
    pub(super) actor_claim: Option<&'a ActorClaim>,
    pub(super) binding: &'a TurnBinding,
    /// Compaction before a round, Summary for `/summarize`.
    pub(super) fold: FixedSystemAgent,
    /// Model a call without its own uses; `None` when no summarizer is configured.
    pub(super) summary_model: Option<&'a ModelRef>,
}

#[async_trait]
impl ModelHost for TurnModels<'_> {
    fn fold_agent(&self) -> Result<FoldAgent, ToolError> {
        let definition = fixed_system_agent(self.binding, self.fold)
            .map_err(|error| ToolError::Other(error.to_string()))?;
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

    async fn complete(&self, session: SessionId, call: ModelCall) -> Result<String, ToolError> {
        let model = call
            .model
            .or_else(|| self.summary_model.cloned())
            .ok_or_else(|| ToolError::Other("no summarizer model is configured".to_string()))?;
        let request = CompletionRequest {
            model: model.clone(),
            system: call.system,
            messages: call.messages,
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: call.max_output_tokens,
            reasoning: call.reasoning.as_deref().and_then(ReasoningEffort::parse),
            headers: Default::default(),
        };
        let stream = self
            .engine
            .provider_router()
            .stream(request, SessionId::new(), MessageId::new())
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        let (text, tokens) = crate::compaction::collect_text_and_usage(stream).await;
        let usage = crate::compaction::UsageCollector::default();
        if let Some(tokens) = tokens.filter(|tokens| !tokens.is_zero()) {
            usage.record(model, tokens);
        }
        self.engine
            .record_side_call_usage(self.actor_claim, session, call.purpose, &usage)
            .await;
        text.map_err(|error| ToolError::Other(error.to_string()))
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
            .map_err(|error| ToolError::Other(error.to_string()))?;
        Ok(window.map(|window| hya_provider::format_responses_compact_system(&window.items)))
    }
}

/// The messages a full-mode round sends: the agent's rendering of the
/// projection, with its prompt images loaded from the blob store.
#[expect(dead_code, reason = "constructed once the compaction bundle is wired")]
pub(super) struct TurnTranscript<'a> {
    pub(super) engine: &'a SessionEngine,
    pub(super) session: SessionId,
    pub(super) agent: &'a AgentSpec,
    pub(super) model: &'a ModelRef,
}

#[async_trait]
impl Transcript for TurnTranscript<'_> {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        let attachments = self
            .engine
            .attachment_data(self.session, projection)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        Ok(projection_to_messages(
            self.agent,
            projection,
            self.model,
            &attachments,
        ))
    }
}
