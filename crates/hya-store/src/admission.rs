use hya_proto::{ActorEpoch, OperationId, SessionId, ToolCallId, now_millis};
use sqlx::Row;

use crate::resident_claim::fence_actor_claim;
use crate::{ActorClaim, SessionStore, StoreError, decode_session_key};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionState {
    Accepted,
    Started,
    Completed,
    Cancelled,
    Aborted,
}

impl AdmissionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Aborted => "aborted",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "started" => Ok(Self::Started),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "aborted" => Ok(Self::Aborted),
            other => Err(StoreError::AdmissionData(format!(
                "unknown admission state `{other}`"
            ))),
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Aborted)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionClaim {
    pub operation_id: OperationId,
    pub source_tool_call_id: ToolCallId,
    pub root_session: SessionId,
    pub request_fingerprint: [u8; 32],
    pub admission_units: u32,
    pub actor_claim: Option<ActorClaim>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionActorBinding {
    pub actor_id: SessionId,
    pub actor_epoch: ActorEpoch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionRecord {
    pub operation_id: OperationId,
    pub source_tool_call_id: ToolCallId,
    pub root_session: SessionId,
    pub request_fingerprint: [u8; 32],
    pub state: AdmissionState,
    pub admission_units: u32,
    pub actor: Option<AdmissionActorBinding>,
    pub logical_released: bool,
    pub terminal_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionClaimOutcome {
    Claimed(AdmissionRecord),
    Existing(AdmissionRecord),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionStartOutcome {
    Started(AdmissionRecord),
    Existing(AdmissionRecord),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionTerminal {
    Completed,
    Cancelled,
    Aborted,
}

impl AdmissionTerminal {
    fn state(self) -> AdmissionState {
        match self {
            Self::Completed => AdmissionState::Completed,
            Self::Cancelled => AdmissionState::Cancelled,
            Self::Aborted => AdmissionState::Aborted,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionFinalizeOutcome {
    pub record: AdmissionRecord,
    /// True only for the process that terminalized a started/debited operation.
    pub release_required: bool,
}

impl SessionStore {
    pub async fn claim_admission(
        &self,
        claim: &AdmissionClaim,
    ) -> Result<AdmissionClaimOutcome, StoreError> {
        if claim.admission_units == 0 {
            return Err(StoreError::AdmissionData(
                "admission units must be greater than zero".to_string(),
            ));
        }
        let now = now_millis();
        let mut tx = self.pool.begin().await?;
        if let Some(actor_claim) = &claim.actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        let actor_id = claim
            .actor_claim
            .as_ref()
            .map(|actor| actor.actor_id.storage_key());
        let actor_epoch = claim
            .actor_claim
            .as_ref()
            .map(|actor| i64::try_from(actor.epoch.get()))
            .transpose()
            .map_err(|_| {
                StoreError::AdmissionData("actor epoch exceeds SQLite INTEGER range".to_string())
            })?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO admission_journal \
             (operation_id, source_tool_call_id, root_session_id, request_fingerprint, state, \
              admission_units, logical_released, created_at, updated_at, actor_id, actor_epoch) \
             VALUES (?, ?, ?, ?, 'accepted', ?, 0, ?, ?, ?, ?)",
        )
        .bind(claim.operation_id.as_uuid().as_bytes().as_slice())
        .bind(claim.source_tool_call_id.as_uuid().as_bytes().as_slice())
        .bind(claim.root_session.storage_key())
        .bind(claim.request_fingerprint.as_slice())
        .bind(i64::from(claim.admission_units))
        .bind(now)
        .bind(now)
        .bind(actor_id)
        .bind(actor_epoch)
        .execute(&mut *tx)
        .await?;

        let record = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch \
             FROM admission_journal WHERE operation_id = ?",
        )
        .bind(claim.operation_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .map(decode_record)
        .transpose()?;
        let Some(record) = record else {
            return Err(StoreError::OperationIdConflict {
                operation_id: claim.operation_id,
            });
        };
        if !record.matches_claim(claim) {
            return Err(StoreError::OperationIdConflict {
                operation_id: claim.operation_id,
            });
        }
        tx.commit().await?;
        if inserted.rows_affected() == 1 {
            Ok(AdmissionClaimOutcome::Claimed(record))
        } else {
            Ok(AdmissionClaimOutcome::Existing(record))
        }
    }

    pub async fn admission(
        &self,
        operation_id: OperationId,
    ) -> Result<Option<AdmissionRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch \
             FROM admission_journal WHERE operation_id = ?",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await?;
        row.map(decode_record).transpose()
    }

    pub async fn start_admission(
        &self,
        operation_id: OperationId,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionStartOutcome, StoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(actor_claim) = actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        let row = sqlx::query(
            "UPDATE admission_journal SET state = 'started', updated_at = ? \
             WHERE operation_id = ? AND state = 'accepted' \
               AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                       actor_id, actor_epoch",
        )
        .bind(now_millis())
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(actor_claim.map(|claim| claim.actor_id.storage_key()))
        .bind(actor_claim.map(|claim| claim.actor_id.storage_key()))
        .bind(
            actor_claim
                .map(|claim| i64::try_from(claim.epoch.get()))
                .transpose()
                .map_err(|_| {
                    StoreError::AdmissionData(
                        "actor epoch exceeds SQLite INTEGER range".to_string(),
                    )
                })?,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = row {
            let record = decode_record(row)?;
            tx.commit().await?;
            return Ok(AdmissionStartOutcome::Started(record));
        }
        let record = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch \
             FROM admission_journal WHERE operation_id = ?",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .map(decode_record)
        .transpose()?
            .ok_or(StoreError::AdmissionNotFound { operation_id })?;
        tx.commit().await?;
        Ok(AdmissionStartOutcome::Existing(record))
    }

    pub async fn finalize_admission(
        &self,
        operation_id: OperationId,
        terminal: AdmissionTerminal,
        reason: &str,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionFinalizeOutcome, StoreError> {
        let target = terminal.state();
        let mut tx = self.pool.begin().await?;
        if let Some(actor_claim) = actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        let row = sqlx::query(
            "UPDATE admission_journal \
             SET state = ?, \
                 logical_released = CASE WHEN state = 'started' THEN 1 ELSE logical_released END, \
                 terminal_reason = ?, updated_at = ? \
             WHERE operation_id = ? \
               AND state IN ('accepted', 'started') \
               AND (? != 'completed' OR state = 'started') \
               AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                       actor_id, actor_epoch",
        )
        .bind(target.as_str())
        .bind(reason)
        .bind(now_millis())
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(target.as_str())
        .bind(actor_claim.map(|claim| claim.actor_id.storage_key()))
        .bind(actor_claim.map(|claim| claim.actor_id.storage_key()))
        .bind(
            actor_claim
                .map(|claim| i64::try_from(claim.epoch.get()))
                .transpose()
                .map_err(|_| {
                    StoreError::AdmissionData(
                        "actor epoch exceeds SQLite INTEGER range".to_string(),
                    )
                })?,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = row {
            let record = decode_record(row)?;
            tx.commit().await?;
            return Ok(AdmissionFinalizeOutcome {
                release_required: record.logical_released,
                record,
            });
        }

        let record = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch \
             FROM admission_journal WHERE operation_id = ?",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .map(decode_record)
        .transpose()?
            .ok_or(StoreError::AdmissionNotFound { operation_id })?;
        if record.state == target {
            tx.commit().await?;
            return Ok(AdmissionFinalizeOutcome {
                record,
                release_required: false,
            });
        }
        let error = StoreError::AdmissionTransitionConflict {
            operation_id,
            from: record.state.as_str(),
            to: target.as_str(),
        };
        tx.rollback().await?;
        Err(error)
    }

    /// Fail-closed startup recovery for non-actor operations. Actor-bound rows
    /// remain for [`Self::abort_recovered_actor_admissions`], which fences them
    /// against the recovered claim. The returned `logical_released` marker is
    /// audit-only: callers must not credit the fresh in-memory governor.
    pub async fn abort_nonterminal_admissions(
        &self,
        reason: &str,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let rows = sqlx::query(
            "UPDATE admission_journal \
             SET state = 'aborted', \
                 logical_released = CASE WHEN state = 'started' THEN 1 ELSE logical_released END, \
                 terminal_reason = ?, updated_at = ? \
             WHERE actor_id IS NULL AND state IN ('accepted', 'started') \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                       actor_id, actor_epoch",
        )
        .bind(reason)
        .bind(now_millis())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_record).collect()
    }

    /// Abort nonterminal operations bound to the epoch fenced by takeover.
    ///
    /// Repeating this call finds no rows, so an in-memory debit can be released
    /// at most once for each recovered operation.
    pub async fn abort_recovered_actor_admissions(
        &self,
        recovered: &crate::RecoveredActorClaim,
        reason: &str,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let mut tx = self.pool.begin().await?;
        fence_actor_claim(&mut tx, &recovered.claim).await?;
        let records =
            abort_recovered_actor_admissions_in_transaction(&mut tx, recovered, reason).await?;
        tx.commit().await?;
        Ok(records)
    }

    pub async fn nonterminal_admissions_for_root(
        &self,
        root_session: SessionId,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch \
             FROM admission_journal \
             WHERE root_session_id = ? AND actor_id IS NULL \
               AND state IN ('accepted', 'started') \
             ORDER BY created_at, operation_id",
        )
        .bind(root_session.storage_key())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_record).collect()
    }
}

pub(crate) async fn abort_recovered_actor_admissions_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    recovered: &crate::RecoveredActorClaim,
    reason: &str,
) -> Result<Vec<AdmissionRecord>, StoreError> {
    let previous_epoch = i64::try_from(recovered.previous_epoch.get()).map_err(|_| {
        StoreError::AdmissionData("actor epoch exceeds SQLite INTEGER range".to_string())
    })?;
    let rows = sqlx::query(
        "UPDATE admission_journal \
         SET state = 'aborted', \
             logical_released = CASE WHEN state = 'started' THEN 1 ELSE logical_released END, \
             terminal_reason = ?, updated_at = ? \
         WHERE actor_id = ? AND actor_epoch <= ? AND state IN ('accepted', 'started') \
         RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                   state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                   actor_id, actor_epoch",
    )
    .bind(reason)
    .bind(now_millis())
    .bind(recovered.claim.actor_id.storage_key())
    .bind(previous_epoch)
    .fetch_all(&mut **tx)
    .await?;
    rows.into_iter()
        .map(decode_record)
        .collect::<Result<Vec<_>, _>>()
}

impl AdmissionRecord {
    fn matches_claim(&self, claim: &AdmissionClaim) -> bool {
        self.operation_id == claim.operation_id
            && self.source_tool_call_id == claim.source_tool_call_id
            && self.root_session == claim.root_session
            && self.request_fingerprint == claim.request_fingerprint
            && self.admission_units == claim.admission_units
            && self.actor
                == claim.actor_claim.map(|actor| AdmissionActorBinding {
                    actor_id: actor.actor_id,
                    actor_epoch: actor.epoch,
                })
    }
}

pub(crate) fn decode_record(row: sqlx::sqlite::SqliteRow) -> Result<AdmissionRecord, StoreError> {
    let operation_id: Vec<u8> = row.try_get("operation_id")?;
    let source_tool_call_id: Vec<u8> = row.try_get("source_tool_call_id")?;
    let root_session_id: Vec<u8> = row.try_get("root_session_id")?;
    let request_fingerprint: Vec<u8> = row.try_get("request_fingerprint")?;
    let state: String = row.try_get("state")?;
    let admission_units: i64 = row.try_get("admission_units")?;
    let logical_released: i64 = row.try_get("logical_released")?;
    let actor_id: Option<Vec<u8>> = row.try_get("actor_id")?;
    let actor_epoch: Option<i64> = row.try_get("actor_epoch")?;
    let fingerprint: [u8; 32] = request_fingerprint.try_into().map_err(|_| {
        StoreError::AdmissionData("request fingerprint must contain 32 bytes".to_string())
    })?;
    let root_session = decode_session_key(&root_session_id)
        .ok_or_else(|| StoreError::AdmissionData("invalid root session key".to_string()))?;
    let operation_uuid = uuid::Uuid::from_slice(&operation_id)
        .map_err(|error| StoreError::AdmissionData(format!("invalid operation id: {error}")))?;
    let source_tool_call_uuid = uuid::Uuid::from_slice(&source_tool_call_id)
        .map_err(|error| StoreError::AdmissionData(format!("invalid tool call id: {error}")))?;
    let actor = match (actor_id, actor_epoch) {
        (None, None) => None,
        (Some(actor_id), Some(actor_epoch)) => {
            let actor_id = decode_session_key(&actor_id).ok_or_else(|| {
                StoreError::AdmissionData("invalid actor session key".to_string())
            })?;
            let actor_epoch = u64::try_from(actor_epoch)
                .ok()
                .filter(|value| *value > 0)
                .map(ActorEpoch::from_storage)
                .ok_or_else(|| StoreError::AdmissionData("invalid actor epoch".to_string()))?;
            Some(AdmissionActorBinding {
                actor_id,
                actor_epoch,
            })
        }
        _ => {
            return Err(StoreError::AdmissionData(
                "actor id and epoch must both be present or absent".to_string(),
            ));
        }
    };

    Ok(AdmissionRecord {
        operation_id: OperationId::from_storage_uuid(operation_uuid),
        source_tool_call_id: ToolCallId::from_uuid(source_tool_call_uuid),
        root_session,
        request_fingerprint: fingerprint,
        state: AdmissionState::parse(&state)?,
        admission_units: u32::try_from(admission_units).map_err(|_| {
            StoreError::AdmissionData("admission units exceed u32 range".to_string())
        })?,
        actor,
        logical_released: logical_released != 0,
        terminal_reason: row.try_get("terminal_reason")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
