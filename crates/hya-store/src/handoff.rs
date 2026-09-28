//! Hot-reload handoff checkpoints (`hya serve restart` invoked inside a shell
//! turn).
//!
//! Invariant: an atomic checkpoint — [`SessionStore::checkpoint_for_handoff`]
//! — closes the session's open assistant turn with
//! [`FinishCause::Handoff`] and inserts one [`PendingResume`] row in the
//! same writer transaction, so the log and the queued resume can never
//! disagree. The checkpoint refuses a turn whose tool parts are still open
//! ([`StoreError::HandoffBoundaryUnsafe`]): a boundary is only safe once the
//! previous round's tools completed, and it never touches member rows —
//! members stay live across the cutover and are revived by the successor's
//! resident recovery.
//!
//! The process that drives the continuation acknowledges the row exactly once
//! ([`SessionStore::begin_handoff_resume`]): the same writer transaction marks
//! the row taken and appends a durable `SessionStatus` continuation marker to
//! the log. `taken_at` is the durable evidence that the resume was delivered,
//! so a successor that itself dies mid-resume is never resumed twice — the
//! turn closes as `interrupted` by ordinary crash recovery and nobody re-runs
//! it automatically. Taken rows are kept as durable evidence.
//!
//! The row carries the checkpointing process's [`OwnerRunId`] and its
//! caller-supplied `generation` (a monotonic handoff generation). Live rows
//! drain oldest generation first; the ack takes a row whoever checkpointed it,
//! because an aborted handoff re-drives its own checkpoints in-process.

use hya_proto::{
    Envelope, Event, FinishCause, OwnerRunId, PartProjection, ResumeId, Role, SessionId,
    ToolPartState, WorkflowRunStatus, now_millis,
};
use sqlx::Row;
use uuid::Uuid;

use crate::recovery::open_turn_terminal_events;
use crate::{
    SessionStore, StoreError, append_event_in_transaction, decode_session_key, replay_projection,
};

/// Reason text written on tool parts closed by a handoff checkpoint (only
/// parts already closed by the dying round leave none; a checkpoint with open
/// parts is refused).
pub const HANDOFF_REASON: &str =
    "handed off: the runtime is restarting and a successor will resume this session";

/// One queued resume recorded by [`SessionStore::checkpoint_for_handoff`] and
/// acknowledged by [`SessionStore::begin_handoff_resume`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResume {
    /// Row identity (`resume_` + UUIDv7).
    pub id: ResumeId,
    /// Session the successor must resume.
    pub session: SessionId,
    /// Owner run that checkpointed (the predecessor; an in-process re-drive
    /// acks its own rows).
    pub owner: OwnerRunId,
    /// The checkpointing process's monotonic handoff generation.
    pub generation: u64,
    /// Queued input for the successor's resumed turn (no new prompt).
    pub prompt: String,
    /// Unix-epoch milliseconds when the checkpoint committed.
    pub created_at: i64,
}

/// One atomic handoff checkpoint: the resume row plus the terminal events
/// appended in the same transaction (empty when no assistant turn was open).
/// The caller publishes `envelopes` to live subscribers.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoffCheckpoint {
    /// The queued resume the successor will take.
    pub resume: PendingResume,
    /// Terminal events (`ToolError`, one `MessageFinished
    /// { Cancelled, cause: handoff }` per open message) appended atomically
    /// with the resume row.
    pub envelopes: Vec<Envelope>,
}

/// One acknowledged handoff resume: the consumed row plus the durable
/// continuation-start marker appended in the same transaction. The caller
/// publishes the marker envelope to live subscribers and then drives the
/// continuation turn.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoffResumeStart {
    /// The resume row this process consumed.
    pub resume: PendingResume,
    /// `SessionStatus` continuation marker appended atomically with the ack.
    pub envelope: Envelope,
}

impl SessionStore {
    /// Checkpoint `session` for a handoff to a successor process: close its
    /// open assistant turn with `FinishCause::Handoff` and queue `prompt` as
    /// the pending resume, in one writer transaction. A session with an
    /// untaken resume is refused ([`StoreError::ResumeAlreadyPending`]) so a
    /// successor is never queued twice, and a turn whose tool parts are still
    /// pending or running is refused
    /// ([`StoreError::HandoffBoundaryUnsafe`]) — the boundary must carry
    /// completed tool results, never error them away. Member rows are left
    /// exactly as they are.
    ///
    /// Requires the runtime-owner claim (the checkpointing process is the
    /// current single writer).
    ///
    /// # Errors
    /// Returns [`StoreError::RuntimeOwnerClaimRequired`] without the claim,
    /// [`StoreError::ResumeAlreadyPending`] when a live resume exists,
    /// [`StoreError::HandoffBoundaryUnsafe`] with open tool parts, or
    /// SQLite / decode failures.
    pub async fn checkpoint_for_handoff(
        &self,
        session: SessionId,
        owner: OwnerRunId,
        generation: u64,
        prompt: String,
    ) -> Result<HandoffCheckpoint, StoreError> {
        self.require_runtime_owner(owner)?;
        let generation_i64 = i64::try_from(generation)
            .map_err(|_| StoreError::ResumeData("generation exceeds i64".to_string()))?;
        // Fold outside the writer transaction first so the (possibly first)
        // full replay lands in the cache; the transaction folds only the tail.
        self.warm_projection(session).await?;
        let created_at = now_millis();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // One live resume per session: take the write lock, then check, so a
        // racing checkpoint for the same session cannot slip between.
        let conflict =
            sqlx::query("SELECT 1 FROM pending_resume WHERE session_id = ? AND taken_at IS NULL")
                .bind(session.storage_key())
                .fetch_optional(&mut *tx)
                .await?;
        if conflict.is_some() {
            return Err(StoreError::ResumeAlreadyPending { session });
        }
        let projection = replay_projection(&self.projections, &mut tx, session).await?;
        // The boundary is safe only after the round's tools completed: an open
        // tool part would be errored away here and its result lost.
        let open_tool_parts = projection
            .session
            .messages
            .iter()
            .filter(|message| message.role == Role::Assistant && message.finish.is_none())
            .flat_map(|message| message.parts.iter())
            .any(|part| {
                matches!(
                    part,
                    PartProjection::Tool {
                        state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                        ..
                    }
                )
            });
        if open_tool_parts {
            return Err(StoreError::HandoffBoundaryUnsafe { session });
        }
        // Members are never closed by a handoff: they stay live across the
        // cutover and the successor's resident recovery revives them.
        let events = open_turn_terminal_events(
            session,
            &projection,
            HANDOFF_REASON,
            "HANDOFF",
            Some(FinishCause::Handoff),
            false,
        );
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        if !envelopes.is_empty() {
            // Every open message is closed now; any index row left for this
            // session names a message the log does not hold open (stale).
            sqlx::query("DELETE FROM open_assistant_message WHERE session_id = ?")
                .bind(session.storage_key())
                .execute(&mut *tx)
                .await?;
        }
        let resume = PendingResume {
            id: ResumeId::new(),
            session,
            owner,
            generation,
            prompt,
            created_at,
        };
        sqlx::query(
            "INSERT INTO pending_resume \
                 (id, session_id, owner_run, generation, prompt, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(resume.id.as_uuid().to_string())
        .bind(session.storage_key())
        .bind(owner.as_uuid().as_bytes().as_slice())
        .bind(generation_i64)
        .bind(&resume.prompt)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(HandoffCheckpoint { resume, envelopes })
    }

    /// Acknowledge `session`'s pending resume and durably record that its
    /// continuation started, in one writer transaction: the live
    /// `pending_resume` row (whoever checkpointed it) is marked taken and one
    /// `SessionStatus` continuation marker is appended to the log. `None`
    /// means nothing was pending — the caller must not drive a continuation.
    ///
    /// Requires the runtime-owner claim: only the current single writer
    /// resumes. At-most-once: the write lock plus the live-row predicate make
    /// a second concurrent ack take `None`.
    ///
    /// # Errors
    /// Returns [`StoreError::RuntimeOwnerClaimRequired`] without the claim,
    /// or SQLite / decode failures.
    pub async fn begin_handoff_resume(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Option<HandoffResumeStart>, StoreError> {
        self.require_runtime_owner(owner)?;
        let taken_at = now_millis();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "UPDATE pending_resume SET taken_by = ?, taken_at = ? \
             WHERE id = ( \
                 SELECT id FROM pending_resume \
                 WHERE session_id = ? AND taken_at IS NULL \
                 ORDER BY generation, created_at, id LIMIT 1 \
             ) \
             RETURNING id, session_id, owner_run, generation, prompt, created_at",
        )
        .bind(owner.as_uuid().as_bytes().as_slice())
        .bind(taken_at)
        .bind(session.storage_key())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let resume = decode_pending_resume(&row)?;
        // Durable continuation start: a `SessionStatus` ping that moves the
        // event tail past the handoff close without touching the transcript.
        // If this process dies before the continuation turn opens its own
        // message, the marker plus the taken row prove the resume was
        // delivered, so no later pass re-runs it.
        let envelope = append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionStatus {
                session,
                status: serde_json::json!({
                    "reason": "handoff-resume",
                    "resume": resume.id.as_uuid().to_string(),
                    "generation": resume.generation,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Some(HandoffResumeStart { resume, envelope }))
    }

    /// Every live (untaken) pending resume, oldest generation first. The
    /// discovery side of the handoff: a successor sees exactly what would be
    /// resumed, without consuming anything.
    ///
    /// # Errors
    /// Returns SQLite / decode failures.
    pub async fn list_pending_resumes(&self) -> Result<Vec<PendingResume>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, session_id, owner_run, generation, prompt, created_at \
             FROM pending_resume WHERE taken_at IS NULL \
             ORDER BY generation, created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(decode_pending_resume).collect()
    }

    /// Sessions whose folded projection still carries a nonterminal
    /// (`Running`) Workflow run. A cutover cannot carry one: the run is driven
    /// by the old process, and the successor's startup recovery terminalizes
    /// nonterminal runs. A restart handoff must be rejected while any exists.
    ///
    /// # Errors
    /// Returns SQLite / decode failures.
    pub async fn sessions_with_running_workflows(&self) -> Result<Vec<SessionId>, StoreError> {
        // serde writes the `type` tag first, so the LIKE prefix rejects almost
        // every row without parsing JSON; `json_extract` keeps the match exact
        // (the same shape the Workflow recovery scan uses). The folded
        // projection decides.
        let rows = sqlx::query(
            "SELECT DISTINCT session_id FROM event_log \
             WHERE payload LIKE '{\"type\":\"workflow_run_started\"%' \
               AND json_extract(payload, '$.type') = 'workflow_run_started' \
             ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .filter_map(|row| {
            let key = row.try_get::<Vec<u8>, _>("session_id").ok()?;
            decode_session_key(&key)
        })
        .collect::<Vec<_>>();
        let mut running = Vec::new();
        for session in rows {
            let projection = self.read_projection(session).await?;
            if projection
                .session
                .workflow
                .as_ref()
                .and_then(|workflow| workflow.run.as_ref())
                .is_some_and(|run| run.status == WorkflowRunStatus::Running)
            {
                running.push(session);
            }
        }
        Ok(running)
    }
}

/// Decode one `pending_resume` row into a [`PendingResume`].
fn decode_pending_resume(row: &sqlx::sqlite::SqliteRow) -> Result<PendingResume, StoreError> {
    let id: String = row.try_get("id")?;
    let id = Uuid::parse_str(&id)
        .map(ResumeId::from_uuid)
        .map_err(|_| StoreError::ResumeData(format!("invalid resume id {id}")))?;
    let session_key: Vec<u8> = row.try_get("session_id")?;
    let session = decode_session_key(&session_key)
        .ok_or_else(|| StoreError::ResumeData("invalid session key".to_string()))?;
    let owner_run: Vec<u8> = row.try_get("owner_run")?;
    let owner = decode_owner_run(&owner_run)?;
    let generation: i64 = row.try_get("generation")?;
    let generation = u64::try_from(generation)
        .map_err(|_| StoreError::ResumeData(format!("negative generation {generation}")))?;
    Ok(PendingResume {
        id,
        session,
        owner,
        generation,
        prompt: row.try_get("prompt")?,
        created_at: row.try_get("created_at")?,
    })
}

/// Decode a 16-byte `owner_run` BLOB into an [`OwnerRunId`].
fn decode_owner_run(bytes: &[u8]) -> Result<OwnerRunId, StoreError> {
    Uuid::from_slice(bytes)
        .map(OwnerRunId::from_storage)
        .map_err(|_| StoreError::ResumeData("invalid owner run id".to_string()))
}
