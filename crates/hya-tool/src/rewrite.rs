//! The rewrite contract between the host and a native bundle (ADR-0033): the
//! bundle-implemented [`Rewriter`] and the host services it calls.
//!
//! A rewriter changes the live Context only by appending events through
//! [`SessionLog`]; the host rebuilds the next request from the projection.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::context::ReasoningReplayPolicy;
use hya_proto::{Event, Message, MessageId, ModelRef, Projection, SessionId};
use serde_json::Value;

use crate::ToolError;

/// A session's event log: the projection and batch appends.
#[async_trait]
pub trait SessionLog: Send + Sync {
    /// The session's current projection.
    async fn projection(&self, session: SessionId) -> Result<Arc<Projection>, ToolError>;
    /// Append `events` as one batch; the host applies its actor claim and
    /// publishes them.
    async fn append(&self, session: SessionId, events: Vec<Event>) -> Result<(), ToolError>;
}

/// The messages a round sends for a projection. Only the host can build them:
/// they depend on the turn's agent and on prompt images in its blob store.
#[async_trait]
pub trait Transcript: Send + Sync {
    /// The request messages `projection` yields for the turn's model.
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError>;
}

/// The window of the model a round runs on.
#[derive(Clone, Copy, Debug)]
pub struct ModelWindow {
    /// Context window in tokens.
    pub max_context: u32,
    /// Whether the route reports usage that may anchor token counts.
    pub usage_reporting: bool,
    /// Which reasoning parts the route resends.
    pub reasoning_replay: ReasoningReplayPolicy,
}

/// The agent that folds context for this rewrite: its model, prompt, and effort.
#[derive(Clone, Debug, Default)]
pub struct FoldAgent {
    /// Model the agent's policy resolves to; `None` leaves the choice to the call.
    pub model: Option<ModelRef>,
    /// The agent's system prompt.
    pub system: Option<String>,
    /// The agent's reasoning effort (`ReasoningEffort` spelling).
    pub reasoning: Option<String>,
}

/// One summary the host's summarizer writes.
#[derive(Clone, Debug, Default)]
pub struct SummaryCall {
    /// Model to call; `None` uses the summarizer's own model.
    pub model: Option<ModelRef>,
    /// System prompt.
    pub system: Option<String>,
    /// Reasoning effort (`ReasoningEffort` spelling).
    pub reasoning: Option<String>,
    /// The summary this one updates, when the session has one.
    pub previous_summary: Option<String>,
    /// Output cap.
    pub max_output_tokens: Option<u32>,
    /// Write a handoff document over the messages verbatim instead of a
    /// summary of them.
    pub handoff: bool,
}

/// Model calls a rewriter makes through the host.
#[async_trait]
pub trait ModelHost: Send + Sync {
    /// The agent that folds context for this rewrite; `Err` when its
    /// definition is missing.
    fn fold_agent(&self) -> Result<FoldAgent, ToolError>;
    /// Whether the host has a summarizer; without one [`Self::summarize`]
    /// fails.
    fn has_summarizer(&self) -> bool;
    /// Summarize `messages` with the host's summarizer, which renders the
    /// prompt. The host bills the call's usage to `session` as compaction,
    /// also when the call fails.
    async fn summarize(
        &self,
        session: SessionId,
        messages: &[Message],
        call: SummaryCall,
    ) -> Result<String, ToolError>;
    /// Compact `messages` with the route's native compaction
    /// (`/responses/compact`), as the marker body to commit; `None` when the
    /// route has none.
    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<Option<String>, ToolError>;
}

/// The two compaction hook points of the host's hook chain.
#[async_trait]
pub trait CompactionHooks: Send + Sync {
    /// `compaction.before` for an overflow at `tokens`: replacement summarizer
    /// instructions when a hook supplied them.
    async fn before(&self, session: SessionId, tokens: usize) -> Option<String>;
    /// `compaction.after` with the committed summary's estimated size.
    async fn after(&self, session: SessionId, summary_tokens: usize);
}

/// What a rewriter sees of the round it runs before.
#[derive(Clone, Copy)]
pub struct RewriteCtx<'a> {
    /// Session being rewritten.
    pub session: SessionId,
    /// The turn's assistant message; for [`Rewriter::rewrite_now`], the
    /// session's newest message.
    pub message: MessageId,
    /// The round's model.
    pub model: &'a ModelRef,
    /// The projection the round would send.
    pub projection: &'a Projection,
    /// The round model's window, when its route advertises one.
    pub window: Option<ModelWindow>,
    /// Estimated tokens of the system prompt and offered tool schemas.
    pub overhead_tokens: usize,
    /// The session's working directory (spill target).
    pub workdir: &'a Path,
    /// The host's settings for this rewriter, as JSON; the bundle owns their
    /// schema.
    pub config: &'a Value,
    /// The session log.
    pub log: &'a dyn SessionLog,
    /// The turn's transcript builder.
    pub transcript: &'a dyn Transcript,
    /// Side model calls; `None` when the host offers none.
    pub models: Option<&'a dyn ModelHost>,
    /// Compaction hooks; `None` when no hook is active.
    pub hooks: Option<&'a dyn CompactionHooks>,
}

/// Rewrites the Context through the log; implemented by a native bundle or
/// by the kernel's own harness trigger. Stateless: everything it needs is in
/// the log or the context.
#[async_trait]
pub trait Rewriter: Send + Sync {
    /// Rewrite before the round, if needed; `true` when the log changed.
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError>;
    /// Rewrite now, on request (`/summarize`); `true` when the log changed.
    async fn rewrite_now(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError>;
}
