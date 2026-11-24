//! Full mode's side of the rewrite contract (`hya_context::rewrite`): the
//! services the kernel's reduction and its extensions (the `hya/compaction`
//! bundle) call back into.
//!
//! The contract carries host failures as `ToolError`. Each service here also
//! records the original [`CoreError`] in [`HostErrors`], so a failed rewrite
//! ends the turn with that error (a stale actor claim) rather than a
//! flattened one.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hya_context::rewrite::{
    CompactionHooks, ModelHost, ModelWindow, SessionLog, ToolError, Transcript,
};
use hya_proto::{Event, Message, ModelRef, Projection, SessionId};
use hya_provider::ProviderRouter;
use hya_store::ActorClaim;

use super::super::summary::summary_messages;
use super::super::{AgentSpec, SessionEngine};
use super::messages::{AttachmentData, projection_to_messages};
use crate::error::CoreError;
use crate::hooks::{CompactionAfterInput, CompactionBeforeInput, CompactionTrigger};

impl SessionEngine {
    /// The settings `RewriteCtx::config` carries to the compaction bundle:
    /// the kept tail and the token-accounting mode.
    pub(in crate::engine) fn rewriter_config(&self) -> serde_json::Value {
        serde_json::json!({
            "keep_recent": self.rewriter_keep_recent,
            "token_accounting": self.token_accounting.mode().as_str(),
        })
    }
}

/// The window of `model`'s route, when it advertises one.
pub(in crate::engine) fn model_window(
    router: &ProviderRouter,
    model: &ModelRef,
) -> Option<ModelWindow> {
    router.capabilities(model).map(|caps| ModelWindow {
        max_context: caps.max_context,
        usage_reporting: caps.usage_reporting,
        reasoning_replay: router.reasoning_replay_policy(model),
        sends_tool_images: router.sends_tool_images(model),
    })
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
/// reduction runs only on overflow and has no summarizer prompt to replace,
/// so a hook's reply is ignored.
#[async_trait]
impl CompactionHooks for SessionEngine {
    async fn before(&self, session: SessionId, tokens: usize) {
        if let Some(hooks) = self.active_hook_dispatcher(session) {
            hooks
                .compaction_before(CompactionBeforeInput {
                    session,
                    trigger: CompactionTrigger::Overflow,
                    messages_token_estimate: tokens,
                })
                .await;
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

/// A rewrite's side model call, the route's native compaction.
pub(in crate::engine) struct TurnModels<'a> {
    pub(in crate::engine) router: &'a ProviderRouter,
    pub(in crate::engine) errors: &'a HostErrors,
}

#[async_trait]
impl ModelHost for TurnModels<'_> {
    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
    ) -> Result<Option<String>, ToolError> {
        let window = self
            .router
            .compact_if_supported(model, messages, None)
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
