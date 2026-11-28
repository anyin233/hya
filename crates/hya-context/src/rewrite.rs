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

use crate::tokens::TokenAccountingMode;

pub use hya_tool_error::ToolError;

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
    /// Whether the route encodes tool-result attachments as images.
    pub sends_tool_images: bool,
}

/// Model calls a rewriter makes through the host.
#[async_trait]
pub trait ModelHost: Send + Sync {
    /// Compact `messages` with `model`'s route-native compaction
    /// (`/responses/compact`), as the marker body to commit; `None` when the
    /// route has none.
    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
    ) -> Result<Option<String>, ToolError>;
}

/// The two compaction hook points of the host's hook chain.
#[async_trait]
pub trait CompactionHooks: Send + Sync {
    /// `compaction.before` for an overflow at `tokens`.
    async fn before(&self, session: SessionId, tokens: usize);
    /// `compaction.after` with the committed marker's estimated size.
    async fn after(&self, session: SessionId, summary_tokens: usize);
}

/// The size of the request a round would send against the rewrite threshold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pressure {
    /// Tokens the request occupies: the larger of the last recorded round's
    /// `prompt + output` and an estimate of the request.
    pub used: u64,
    /// Tokens above which the context is reduced (80% of the window); `0`
    /// for a reduction on request.
    pub threshold: u64,
}

impl Pressure {
    /// Whether the request is over the threshold.
    #[must_use]
    pub fn over(self) -> bool {
        self.used > self.threshold
    }
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
    /// The request's size against the threshold, set by the kernel; `None`
    /// without a window.
    pub pressure: Option<Pressure>,
    /// The session's working directory (spill target).
    pub workdir: &'a Path,
    /// How the host measures window occupancy (`config.yaml`
    /// `context.token_accounting`).
    pub token_accounting: TokenAccountingMode,
    /// The session log.
    pub log: &'a dyn SessionLog,
    /// The turn's transcript builder.
    pub transcript: &'a dyn Transcript,
    /// Side model calls; `None` when the host offers none.
    pub models: Option<&'a dyn ModelHost>,
    /// Compaction hooks; `None` when no hook is active.
    pub hooks: Option<&'a dyn CompactionHooks>,
}

/// A context-reduction extension a native bundle registers. The kernel calls
/// every extension, in registration order, before its own rewrite.
/// Stateless: everything it needs is in the log or the context.
#[async_trait]
pub trait Rewriter: Send + Sync {
    /// Runs before every round; reduces only when `cx.pressure` is over the
    /// threshold. `true` when the log changed.
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError>;
    /// Reduce now, on request (`/summarize`); `true` when the log changed.
    async fn rewrite_now(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError>;
}
