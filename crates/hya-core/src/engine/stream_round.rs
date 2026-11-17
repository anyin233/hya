use async_trait::async_trait;
use hya_proto::{Event, SessionId};
use hya_store::ActorClaim;

use super::SessionEngine;
use crate::error::CoreError;
use crate::kernel::Emit;

/// [`Emit`] for one turn's writer: appends under its actor claim.
pub(super) struct TurnEmit<'a> {
    pub(super) engine: &'a SessionEngine,
    pub(super) actor_claim: Option<&'a ActorClaim>,
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
