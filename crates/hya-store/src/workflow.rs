//! Startup convergence for durable Workflow runs.

use hya_proto::{
    ActorClaim, Envelope, Event, OwnerRunId, Projection, WorkflowRunId, WorkflowRunStatus,
};
use sqlx::Row;

use crate::{
    SessionStore, StoreError, append_event_in_transaction, decode_session_key, replay_projection,
    resident_claim::fence_actor_claim,
};

/// Result of atomically admitting one durable Workflow run.
#[derive(Debug, PartialEq)]
pub enum WorkflowAdmissionOutcome {
    /// The start event was appended by this request.
    Admitted(Box<Envelope>),
    /// This run id was already admitted with the same immutable request hash.
    Existing,
    /// This run id was already admitted with different immutable request data.
    Conflict,
    /// Another run currently owns the Session.
    Busy {
        /// Active run identity.
        run: WorkflowRunId,
    },
}

/// Result of atomically changing one Session's Workflow selection.
#[derive(Debug, PartialEq)]
pub enum WorkflowSelectionOutcome {
    /// The selection event was appended by this request.
    Selected(Box<Envelope>),
    /// A run currently owns the Session, so selection did not change.
    Busy {
        /// Active run identity.
        run: WorkflowRunId,
    },
}

impl SessionStore {
    /// Atomically fence, deduplicate, exclude, and append one Workflow run start.
    ///
    /// # Errors
    /// Returns a typed store error for a malformed event, stale actor claim,
    /// corrupt replay data, or SQLite transaction failure.
    pub async fn admit_workflow_run(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: hya_proto::SessionId,
        event: Event,
    ) -> Result<WorkflowAdmissionOutcome, StoreError> {
        let (run, request_hash) = match &event {
            Event::WorkflowRunStarted {
                session: event_session,
                run,
                request_hash,
                ..
            } if *event_session == session => (*run, request_hash.as_str()),
            _ => {
                return Err(StoreError::WorkflowData(
                    "run admission requires a matching WorkflowRunStarted event".to_string(),
                ));
            }
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let events = replay_workflow_events(&mut tx, session).await?;
        if let Some(existing_hash) = events.iter().find_map(|envelope| match &envelope.event {
            Event::WorkflowRunStarted {
                run: existing,
                request_hash,
                ..
            } if *existing == run => Some(request_hash.as_str()),
            _ => None,
        }) {
            tx.commit().await?;
            return Ok(if existing_hash == request_hash {
                WorkflowAdmissionOutcome::Existing
            } else {
                WorkflowAdmissionOutcome::Conflict
            });
        }
        let projection = Projection::from_events(&events);
        if let Some(active) = projection
            .session
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.run.as_ref())
            && active.status == WorkflowRunStatus::Running
        {
            let run = active.id;
            tx.commit().await?;
            return Ok(WorkflowAdmissionOutcome::Busy { run });
        }
        let envelope = append_event_in_transaction(&mut tx, session, event).await?;
        tx.commit().await?;
        Ok(WorkflowAdmissionOutcome::Admitted(Box::new(envelope)))
    }

    /// Atomically reject selection during an active run and append otherwise.
    ///
    /// # Errors
    /// Returns a typed store error for a malformed event, stale actor claim,
    /// corrupt replay data, or SQLite transaction failure.
    pub async fn select_workflow(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: hya_proto::SessionId,
        event: Event,
    ) -> Result<WorkflowSelectionOutcome, StoreError> {
        if !matches!(&event, Event::WorkflowSelected { session: event_session, .. } if *event_session == session)
        {
            return Err(StoreError::WorkflowData(
                "selection requires a matching WorkflowSelected event".to_string(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let projection = replay_projection(&mut tx, session).await?;
        if let Some(active) = projection
            .session
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.run.as_ref())
            && active.status == WorkflowRunStatus::Running
        {
            let run = active.id;
            tx.commit().await?;
            return Ok(WorkflowSelectionOutcome::Busy { run });
        }
        let envelope = append_event_in_transaction(&mut tx, session, event).await?;
        tx.commit().await?;
        Ok(WorkflowSelectionOutcome::Selected(Box::new(envelope)))
    }

    /// Mark every persisted nonterminal Workflow run as interrupted.
    ///
    /// The store must already hold the matching runtime-owner claim. That
    /// exclusive claim proves another runtime cannot still own the persisted
    /// nonterminal run before owner identities are compared.
    ///
    /// Call this only at backend startup after the prior runtime owner is known
    /// to be gone. The writer transaction folds existing logs and appends one
    /// terminal event per running Session. It never starts or replays a Stage.
    ///
    /// # Errors
    /// Returns a missing/mismatched owner claim, event-log, or SQLite transaction failure.
    pub async fn recover_nonterminal_workflows(
        &self,
        current_owner: OwnerRunId,
        reason: &str,
    ) -> Result<Vec<Envelope>, StoreError> {
        self.require_runtime_owner(current_owner)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query("SELECT DISTINCT session_id FROM event_log ORDER BY session_id")
            .fetch_all(&mut *tx)
            .await?;
        let mut recovered = Vec::new();
        for row in rows {
            let key: Vec<u8> = row.try_get("session_id")?;
            let Some(session) = decode_session_key(&key) else {
                continue;
            };
            let projection = replay_projection(&mut tx, session).await?;
            let Some(run) = projection
                .session
                .workflow
                .and_then(|workflow| workflow.run)
            else {
                continue;
            };
            if run.status != WorkflowRunStatus::Running || run.owner == current_owner {
                continue;
            }
            recovered.push(
                append_event_in_transaction(
                    &mut tx,
                    session,
                    Event::WorkflowRunFinished {
                        session,
                        run: run.id,
                        status: WorkflowRunStatus::Interrupted,
                        error: Some(reason.chars().take(2_048).collect()),
                    },
                )
                .await?,
            );
        }
        tx.commit().await?;
        Ok(recovered)
    }
}

/// Read one Session log inside an existing writer transaction.
async fn replay_workflow_events(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: hya_proto::SessionId,
) -> Result<Vec<Envelope>, StoreError> {
    let rows =
        sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
            .bind(session.storage_key())
            .fetch_all(&mut **tx)
            .await?;
    let mut envelopes = Vec::with_capacity(rows.len());
    for row in rows {
        let seq: i64 = row.try_get("seq")?;
        let ts_millis: i64 = row.try_get("ts")?;
        let payload: String = row.try_get("payload")?;
        envelopes.push(Envelope {
            seq: hya_proto::EventSeq(seq.max(0) as u64),
            ts_millis,
            event: serde_json::from_str(&payload)?,
        });
    }
    Ok(envelopes)
}
