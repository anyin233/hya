use async_trait::async_trait;
use hya_proto::{Event, MessageId, ModelRef, SessionId};
use hya_provider::EventStream;
use hya_store::ActorClaim;

use super::SessionEngine;
use crate::error::CoreError;
use crate::kernel::Emit;
use crate::kernel::round::{self, Failure, Round};

/// [`Emit`] for one turn's writer: appends under its actor claim.
struct TurnEmit<'a> {
    engine: &'a SessionEngine,
    actor_claim: Option<&'a ActorClaim>,
}

#[async_trait]
impl Emit for TurnEmit<'_> {
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError> {
        self.engine
            .emit_batch_for_actor(self.actor_claim, session, events)
            .await
    }

    fn live(&self, event: Event) {
        self.engine.publish_live(event);
    }
}

impl SessionEngine {
    /// Drain one provider round of `step`, served by `model`, through the
    /// kernel, with the `text.complete` hook applied to each text part.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn collect_stream_round(
        &self,
        session: SessionId,
        message: MessageId,
        stream: EventStream,
        actor_claim: Option<&ActorClaim>,
        step: u32,
        model: &ModelRef,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Round, Failure> {
        let emit = TurnEmit {
            engine: self,
            actor_claim,
        };
        round::collect(
            &emit,
            Some(self),
            session,
            message,
            step,
            model,
            stream,
            cancel,
        )
        .await
    }
}
