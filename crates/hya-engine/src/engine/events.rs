use std::sync::Arc;

use hya_proto::{Envelope, Event, EventSeq, Projection, SessionId, now_millis};
use hya_store::ActorClaim;

use super::SessionEngine;
use crate::error::CoreError;
use crate::hooks::activation_hook_for;

impl SessionEngine {
    /// Replay the full ordered event log for `session`.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, CoreError> {
        Ok(self.store.replay(session).await?)
    }

    /// Return whether a Session has a durable event log.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn session_exists(&self, session: SessionId) -> Result<bool, CoreError> {
        Ok(self.store.session_exists(session).await?)
    }

    /// Fold the session log into a projection.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, CoreError> {
        Ok(self.store.read_projection(session).await?)
    }

    /// Shared handle to the session's folded projection, without a deep clone.
    ///
    /// Same fold as [`SessionEngine::read_projection`] (the store's projection
    /// cache); prefer it on hot paths that re-read large team-root logs.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn read_projection_shared(
        &self,
        session: SessionId,
    ) -> Result<Arc<Projection>, CoreError> {
        Ok(self.store.read_projection_shared(session).await?)
    }

    pub(super) async fn emit(&self, session: SessionId, event: Event) -> Result<(), CoreError> {
        self.emit_batch_for_actor(None, session, vec![event]).await
    }

    pub(super) async fn emit_batch_for_actor(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<(), CoreError> {
        let envelopes = match actor_claim {
            Some(claim) => {
                self.store
                    .commit_resident_mutation(claim, session, events)
                    .await?
            }
            None => self.store.append_events(session, events).await?,
        };
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        Ok(())
    }

    /// Append one `UsageRecorded` side-call record per call drained from `usage`.
    ///
    /// Side calls (title, summarizer) carry no message or step and never touch
    /// the transcript. Best-effort: a failed append is logged, never surfaced,
    /// so accounting cannot fail the work it describes.
    pub(crate) async fn record_side_call_usage(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        purpose: hya_proto::UsagePurpose,
        usage: &crate::compaction::UsageCollector,
    ) {
        for (model, tokens) in usage.take() {
            let event = Event::UsageRecorded {
                session,
                message: None,
                step: None,
                model,
                purpose,
                tokens,
            };
            if let Err(error) = self.emit_for_actor(actor_claim, session, event).await {
                tracing::warn!(%session, "side-call usage record was not appended: {error:#}");
            }
        }
    }

    pub(crate) async fn emit_for_actor(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<(), CoreError> {
        match actor_claim {
            Some(claim) => {
                self.commit_resident_mutation(claim, session, vec![event])
                    .await
            }
            None => self.emit(session, event).await,
        }
    }

    pub(crate) async fn validate_actor_claim(
        &self,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        if let Some(claim) = actor_claim {
            self.store.validate_actor_claim(claim).await?;
        }
        Ok(())
    }

    /// Commit resident-owned canonical events only while the supplied actor
    /// capability is current. Publication happens after the SQLite transaction,
    /// so a stale completion cannot advance live observers or replay state.
    pub async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<(), CoreError> {
        self.emit_batch_for_actor(Some(claim), session, events)
            .await
    }

    pub(super) fn publish_live(&self, event: Event) {
        self.publish_envelope(Envelope {
            seq: EventSeq(0),
            ts_millis: now_millis(),
            event,
        });
    }

    pub(crate) fn publish_envelope(&self, envelope: Envelope) {
        if let Some(hooks) = &self.hooks {
            hooks.dispatch_event(&envelope);
        }
        for hooks in self.runtime.global_plugin_hooks() {
            hooks.dispatch_event(&envelope);
        }
        let session = envelope.event.session();
        let active = session.and_then(activation_hook_for);
        if let Some(hooks) = active {
            hooks.dispatch_event(&envelope);
        } else if let Some(hooks) = session.and_then(|session| self.captured_session_hooks(session))
        {
            hooks.dispatch_event(&envelope);
        }
        self.bus.publish(envelope);
    }
}
