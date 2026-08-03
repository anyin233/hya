use hya_proto::{
    ActorClaim, ActorEpoch, Envelope, Event, EventSeq, OwnerRunId, SessionId, now_millis,
};
use sqlx::Row;
use uuid::Uuid;

use crate::admission::decode_record;
use crate::{AdmissionRecord, SessionStore, StoreError, decode_session_key};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredActorClaim {
    pub claim: ActorClaim,
    pub previous_epoch: ActorEpoch,
}

impl SessionStore {
    pub async fn active_actor_ids(&self) -> Result<Vec<SessionId>, StoreError> {
        let rows = sqlx::query(
            "SELECT actor_id FROM resident_actor_claim WHERE state = 'active' ORDER BY actor_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let actor_id: Vec<u8> = row.try_get("actor_id")?;
                decode_session_key(&actor_id).ok_or_else(|| {
                    StoreError::ActorClaimData("invalid actor session key".to_string())
                })
            })
            .collect()
    }

    /// Claim a never-claimed or explicitly released actor.
    ///
    /// The upsert predicate is the concurrency gate: an active row cannot be
    /// overwritten, so simultaneous ordinary claims have exactly one winner.
    pub async fn try_claim_new(
        &self,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<ActorClaim, StoreError> {
        let row = sqlx::query(
            "INSERT INTO resident_actor_claim (actor_id, epoch, owner_run_id, state) \
             VALUES (?, 1, ?, 'active') \
             ON CONFLICT(actor_id) DO UPDATE SET \
                 epoch = resident_actor_claim.epoch + 1, \
                 owner_run_id = excluded.owner_run_id, \
                 state = 'active' \
             WHERE resident_actor_claim.state = 'released' \
               AND resident_actor_claim.epoch < 9223372036854775807 \
             RETURNING actor_id, epoch, owner_run_id",
        )
        .bind(actor_id.storage_key())
        .bind(owner_run_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => decode_claim(row),
            None => Err(StoreError::ActorAlreadyClaimed { actor_id }),
        }
    }

    /// Take over one active actor before runtime readiness.
    ///
    /// The single conditional update is the fence linearization point. The
    /// returned old epoch is derived from the successfully returned `N + 1`, so
    /// no read-before-write race exists.
    pub async fn recover_claim(
        &self,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<RecoveredActorClaim, StoreError> {
        let row = sqlx::query(
            "UPDATE resident_actor_claim SET \
                 epoch = epoch + 1, owner_run_id = ? \
             WHERE actor_id = ? AND state = 'active' \
               AND epoch < 9223372036854775807 \
             RETURNING actor_id, epoch, owner_run_id",
        )
        .bind(owner_run_id.as_uuid().as_bytes().as_slice())
        .bind(actor_id.storage_key())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(StoreError::ActorClaimUnavailable { actor_id });
        };
        let claim = decode_claim(row)?;
        let previous_epoch = ActorEpoch::from_storage(claim.epoch.get() - 1);
        Ok(RecoveredActorClaim {
            claim,
            previous_epoch,
        })
    }

    /// Point-check an execution capability against the current durable owner.
    pub async fn validate_actor_claim(&self, claim: &ActorClaim) -> Result<(), StoreError> {
        let current = sqlx::query(
            "SELECT actor_id, epoch, owner_run_id \
             FROM resident_actor_claim \
             WHERE actor_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .fetch_optional(&self.pool)
        .await?
        .map(decode_claim)
        .transpose()?;
        if current.as_ref() == Some(claim) {
            Ok(())
        } else {
            Err(StoreError::StaleActorClaim {
                actor_id: claim.actor_id,
            })
        }
    }

    /// Release only the exact current capability. Any operation still bound to
    /// this incarnation is terminalized in the same writer transaction, so a
    /// released claim cannot strand nonterminal work. Repeating a matching
    /// release is idempotent; a newer owner/epoch remains fenced.
    pub async fn release_claim(
        &self,
        claim: &ActorClaim,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let epoch = i64::try_from(claim.epoch.get()).map_err(|_| {
            StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
        })?;
        let mut tx = self.pool.begin().await?;
        let current = sqlx::query(
            "UPDATE resident_actor_claim SET state = state \
             WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
        if current.rows_affected() == 0 {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT state FROM resident_actor_claim \
                 WHERE actor_id = ? AND epoch = ? AND owner_run_id = ?",
            )
            .bind(claim.actor_id.storage_key())
            .bind(epoch)
            .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
            .fetch_optional(&mut *tx)
            .await?;
            if state.as_deref() == Some("released") {
                tx.commit().await?;
                return Ok(Vec::new());
            }
            tx.rollback().await?;
            return Err(StoreError::StaleActorClaim {
                actor_id: claim.actor_id,
            });
        }

        let rows = sqlx::query(
            "UPDATE admission_journal \
             SET state = 'aborted', \
                 logical_released = CASE WHEN state = 'started' THEN 1 ELSE logical_released END, \
                 terminal_reason = 'resident actor released', updated_at = ? \
             WHERE actor_id = ? AND actor_epoch = ? AND state IN ('accepted', 'started') \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
        )
        .bind(now_millis())
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .fetch_all(&mut *tx)
        .await?;
        let records = rows
            .into_iter()
            .map(decode_record)
            .collect::<Result<Vec<_>, _>>()?;
        sqlx::query(
            "UPDATE resident_actor_claim SET state = 'released' \
             WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(records)
    }

    /// Atomically fence and append one resident-owned canonical mutation.
    ///
    /// The no-op conditional `UPDATE` acquires SQLite's writer lock and validates
    /// the full capability tuple in the same transaction as the event append.
    /// The caller publishes the returned envelopes only after this transaction
    /// commits.
    pub async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: &[Event],
    ) -> Result<Vec<Envelope>, StoreError> {
        if events.iter().any(|event| event.session() != Some(session)) {
            return Err(StoreError::ActorClaimData(
                "resident mutation event session mismatch".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        fence_actor_claim(&mut tx, claim).await?;

        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            let ts_millis = now_millis();
            let payload = serde_json::to_string(event)?;
            let row = sqlx::query(
                "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq",
            )
            .bind(session.storage_key())
            .bind(payload)
            .bind(ts_millis)
            .fetch_one(&mut *tx)
            .await?;
            let seq: i64 = row.try_get("seq")?;
            envelopes.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis,
                event: event.clone(),
            });
        }
        tx.commit().await?;
        Ok(envelopes)
    }
}

pub(crate) async fn fence_actor_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &ActorClaim,
) -> Result<(), StoreError> {
    let current = sqlx::query(
        "UPDATE resident_actor_claim SET state = state \
         WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active' \
         RETURNING actor_id",
    )
    .bind(claim.actor_id.storage_key())
    .bind(i64::try_from(claim.epoch.get()).map_err(|_| {
        StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
    })?)
    .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    if current.is_some() {
        Ok(())
    } else {
        Err(StoreError::StaleActorClaim {
            actor_id: claim.actor_id,
        })
    }
}

fn decode_claim(row: sqlx::sqlite::SqliteRow) -> Result<ActorClaim, StoreError> {
    let actor_id: Vec<u8> = row.try_get("actor_id")?;
    let epoch: i64 = row.try_get("epoch")?;
    let owner_run_id: Vec<u8> = row.try_get("owner_run_id")?;
    let actor_id = decode_session_key(&actor_id)
        .ok_or_else(|| StoreError::ActorClaimData("invalid actor session key".to_string()))?;
    let epoch = u64::try_from(epoch)
        .ok()
        .filter(|value| *value > 0)
        .map(ActorEpoch::from_storage)
        .ok_or_else(|| StoreError::ActorClaimData("invalid actor epoch".to_string()))?;
    let owner_run_id = Uuid::from_slice(&owner_run_id)
        .map(OwnerRunId::from_storage)
        .map_err(|error| StoreError::ActorClaimData(format!("invalid owner run id: {error}")))?;
    Ok(ActorClaim {
        actor_id,
        epoch,
        owner_run_id,
    })
}
