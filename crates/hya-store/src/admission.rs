//! Durable spawn admission journal: claim, start, finalize, promote, recover.
//!
//! Capacity caps and lifecycle match `docs/architecture/admission-and-governor.md`.

use hya_proto::{ActorEpoch, OperationId, SessionId, ToolCallId, now_millis};
use sqlx::Row;

use crate::resident_claim::fence_actor_claim;
use crate::{ActorClaim, SessionStore, StoreError, decode_session_key};

/// Lifecycle of one admission_journal member row (wire strings match SQL CHECK).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionState {
    /// Parked for FIFO promotion into an active slot.
    Queued,
    /// Durably claimed; governor may debit before start.
    Accepted,
    /// In flight; first terminalize sets `logical_released` for exactly-once refund.
    Started,
    /// Parent suspended while children run; not counted as active.
    Waiting,
    /// Successful terminal state.
    Completed,
    /// Cancelled terminal state.
    Cancelled,
    /// Aborted terminal state (error / recovery).
    Aborted,
}

impl AdmissionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Accepted => "accepted",
            Self::Started => "started",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Aborted => "aborted",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "queued" => Ok(Self::Queued),
            "accepted" => Ok(Self::Accepted),
            "started" => Ok(Self::Started),
            "waiting" => Ok(Self::Waiting),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "aborted" => Ok(Self::Aborted),
            other => Err(StoreError::AdmissionData(format!(
                "unknown admission state `{other}`"
            ))),
        }
    }

    /// Whether this state is terminal (`Completed` / `Cancelled` / `Aborted`).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Aborted)
    }
}

/// Snapshot of journal occupancy against capacity caps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionCounts {
    /// Rows in accepted and started states.
    pub active: u32,
    /// Rows in queued and waiting states.
    pub non_active: u32,
    /// Sum of all nonterminal rows.
    pub total: u32,
}

/// Input for a durable admission claim (single or batch head).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionClaim {
    /// Stable operation id (idempotency key with fingerprint).
    pub operation_id: OperationId,
    /// Tool call that sourced this spawn.
    pub source_tool_call_id: ToolCallId,
    /// Team-root session whose spawn budget is charged.
    pub root_session: SessionId,
    /// 32-byte fingerprint of the immutable request payload.
    pub request_fingerprint: [u8; 32],
    /// Budget units reserved (must be > 0).
    pub admission_units: u32,
    /// Optional resident actor binding for the claim.
    pub actor_claim: Option<ActorClaim>,
}

/// Actor id + epoch stored on an admission row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionActorBinding {
    /// Resident actor session id.
    pub actor_id: SessionId,
    /// Claim epoch that must match for fenced updates.
    pub actor_epoch: ActorEpoch,
}

/// One composite-PK member of the admission journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionRecord {
    /// Operation this member belongs to.
    pub operation_id: OperationId,
    /// Source tool call id.
    pub source_tool_call_id: ToolCallId,
    /// Root session for budget accounting.
    pub root_session: SessionId,
    /// Request fingerprint for conflict detection.
    pub request_fingerprint: [u8; 32],
    /// Index within the batch (`0..batch_size`).
    pub member_ordinal: u32,
    /// Total members in the batch.
    pub batch_size: u32,
    /// Current lifecycle state.
    pub state: AdmissionState,
    /// Units charged for this member/operation.
    pub admission_units: u32,
    /// Optional actor binding.
    pub actor: Option<AdmissionActorBinding>,
    /// Set when a started row is first terminalized (exactly-once refund flag).
    pub logical_released: bool,
    /// Optional terminal reason string.
    pub terminal_reason: Option<String>,
    /// Row creation time (unix millis).
    pub created_at: i64,
    /// Last update time (unix millis).
    pub updated_at: i64,
}

/// Outcome of [`SessionStore::claim_admission`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionClaimOutcome {
    /// New row inserted as `accepted`.
    Claimed(AdmissionRecord),
    /// Matching row already existed (idempotent reclaim).
    Existing(AdmissionRecord),
}

/// Outcome of start (`accepted` → `started`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionStartOutcome {
    /// Transition applied.
    Started(AdmissionRecord),
    /// Row already past accepted (idempotent).
    Existing(AdmissionRecord),
}

/// Terminal classification written by finalize APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionTerminal {
    /// Map to [`AdmissionState::Completed`].
    Completed,
    /// Map to [`AdmissionState::Cancelled`].
    Cancelled,
    /// Map to [`AdmissionState::Aborted`].
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

/// One member after finalize; `release_required` drives governor refund exactly once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionFinalizeOutcome {
    /// Terminalized journal row.
    pub record: AdmissionRecord,
    /// True only for the process that terminalized a started/debited operation.
    pub release_required: bool,
}

impl SessionStore {
    /// Insert a single-member row as `accepted` (`member_ordinal=0`, `batch_size=1`).
    ///
    /// Idempotent: same fingerprint returns [`AdmissionClaimOutcome::Existing`];
    /// conflicting fingerprint → [`StoreError::OperationIdConflict`].
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
              admission_units, logical_released, created_at, updated_at, actor_id, actor_epoch, \
              member_ordinal, batch_size) \
             VALUES (?, ?, ?, ?, 'accepted', ?, 0, ?, ?, ?, ?, 0, 1)",
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
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? AND member_ordinal = 0 AND batch_size = 1",
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

    /// Load the single-member primary record (`member_ordinal=0`, `batch_size=1`), if present.
    pub async fn admission(
        &self,
        operation_id: OperationId,
    ) -> Result<Option<AdmissionRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? AND member_ordinal = 0 AND batch_size = 1",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await?;
        row.map(decode_record).transpose()
    }

    /// Load every member row for an operation id (batch members).
    pub async fn admissions(
        &self,
        operation_id: OperationId,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? \
             ORDER BY member_ordinal",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_record).collect()
    }

    /// Count active and non-active durable admission rows.
    pub async fn admission_counts(&self) -> Result<AdmissionCounts, StoreError> {
        let row = sqlx::query(
            "SELECT COUNT(CASE WHEN state IN ('accepted', 'started') THEN 1 END) AS active, \
                    COUNT(CASE WHEN state IN ('queued', 'waiting') THEN 1 END) AS non_active, \
                    COUNT(CASE WHEN state IN ('queued', 'accepted', 'started', 'waiting') THEN 1 END) AS total \
             FROM admission_journal",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(AdmissionCounts {
            active: u32::try_from(row.try_get::<i64, _>("active")?).map_err(|_| {
                StoreError::AdmissionData("admission active count exceeds u32 range".to_string())
            })?,
            non_active: u32::try_from(row.try_get::<i64, _>("non_active")?).map_err(|_| {
                StoreError::AdmissionData(
                    "admission non-active count exceeds u32 range".to_string(),
                )
            })?,
            total: u32::try_from(row.try_get::<i64, _>("total")?).map_err(|_| {
                StoreError::AdmissionData("admission total count exceeds u32 range".to_string())
            })?,
        })
    }

    /// Move a single-member claim from `accepted` to `started`.
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
             WHERE operation_id = ? AND member_ordinal = 0 AND batch_size = 1 \
               AND state = 'accepted' \
               AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
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
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? AND member_ordinal = 0 AND batch_size = 1",
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

    /// Terminalize a single-member operation; sets `logical_released` when previous state was `started`.
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
               AND member_ordinal = 0 AND batch_size = 1 \
               AND state IN ('accepted', 'started') \
               AND (? != 'completed' OR state = 'started') \
             AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
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
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? AND member_ordinal = 0 AND batch_size = 1",
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

    /// Recover non-actor operations at startup. Complete bound Accepted rows
    /// return to Queued; incomplete Accepted, Started, and previously-started
    /// Waiting rows become Aborted. Waiting rows already released their active
    /// lease at suspension, so they do not release it again. Actor-bound rows
    /// remain for [`Self::abort_recovered_actor_admissions`], which fences them
    /// against the recovered claim.
    pub async fn recover_nonterminal_admissions(
        &self,
        reason: &str,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let rows = sqlx::query(
            "UPDATE admission_journal \
             SET state = CASE \
                     WHEN state = 'started' THEN 'aborted' \
                     WHEN state = 'accepted' \
                          AND runtime_fingerprint_version IS NOT NULL \
                          AND runtime_fingerprint IS NOT NULL \
                          AND admission_binding_fingerprint_version IS NOT NULL \
                          AND admission_binding_fingerprint IS NOT NULL \
                          AND spawn_intent IS NOT NULL THEN 'queued' \
                     ELSE 'aborted' \
                 END, \
                 logical_released = CASE WHEN state = 'started' THEN 1 ELSE 0 END, \
                 terminal_reason = CASE \
                     WHEN state = 'accepted' \
                          AND runtime_fingerprint_version IS NOT NULL \
                          AND runtime_fingerprint IS NOT NULL \
                          AND admission_binding_fingerprint_version IS NOT NULL \
                          AND admission_binding_fingerprint IS NOT NULL \
                          AND spawn_intent IS NOT NULL THEN NULL \
                     ELSE ? \
                 END, \
                 updated_at = ? \
             WHERE actor_id IS NULL AND state IN ('accepted', 'started', 'waiting') \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
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

    /// Load whole operations whose terminal journal rows prove a prior governor debit release.
    ///
    /// `root_session` selects one run budget. The returned operation IDs are safe
    /// to pass to the process-local governor's idempotent release path because
    /// every declared batch member is terminal and at least one started member
    /// set the durable logical-release marker.
    pub async fn terminal_released_operations_for_root(
        &self,
        root_session: SessionId,
    ) -> Result<Vec<OperationId>, StoreError> {
        let rows = sqlx::query(
            "SELECT operation_id \
             FROM admission_journal \
             WHERE root_session_id = ? \
             GROUP BY operation_id \
             HAVING COUNT(*) = MAX(batch_size) \
                AND SUM(CASE WHEN state IN ('completed', 'cancelled', 'aborted') \
                             THEN 0 ELSE 1 END) = 0 \
                AND MAX(logical_released) = 1 \
             ORDER BY MIN(created_at), operation_id",
        )
        .bind(root_session.storage_key())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let operation_id: Vec<u8> = row.try_get("operation_id")?;
                uuid::Uuid::from_slice(&operation_id)
                    .map(OperationId::from_storage_uuid)
                    .map_err(|error| {
                        StoreError::AdmissionData(format!("invalid operation id: {error}"))
                    })
            })
            .collect()
    }

    /// Non-actor rows still `accepted` or `started` for a root session (root-turn cleanup).
    pub async fn nonterminal_admissions_for_root(
        &self,
        root_session: SessionId,
    ) -> Result<Vec<AdmissionRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
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
                   member_ordinal, batch_size, state, admission_units, logical_released, \
                   terminal_reason, created_at, updated_at, actor_id, actor_epoch",
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
            && self.member_ordinal == 0
            && self.batch_size == 1
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
    let member_ordinal: i64 = row.try_get("member_ordinal")?;
    let batch_size: i64 = row.try_get("batch_size")?;
    let state: String = row.try_get("state")?;
    let admission_units: i64 = row.try_get("admission_units")?;
    let logical_released: i64 = row.try_get("logical_released")?;
    let actor_id: Option<Vec<u8>> = row.try_get("actor_id")?;
    let actor_epoch: Option<i64> = row.try_get("actor_epoch")?;
    let member_ordinal = u32::try_from(member_ordinal)
        .map_err(|_| StoreError::AdmissionData("member ordinal exceeds u32 range".to_string()))?;
    let batch_size = u32::try_from(batch_size)
        .map_err(|_| StoreError::AdmissionData("batch size exceeds u32 range".to_string()))?;
    if batch_size == 0 {
        return Err(StoreError::AdmissionData(
            "batch size must be greater than zero".to_string(),
        ));
    }
    if member_ordinal >= batch_size {
        return Err(StoreError::AdmissionData(
            "member ordinal must be less than batch size".to_string(),
        ));
    }
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
        member_ordinal,
        batch_size,
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
