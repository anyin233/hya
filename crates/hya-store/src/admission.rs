//! Durable spawn admission journal: claim, start, finalize, promote, recover.
//!
//! Capacity caps and lifecycle match `docs/architecture/admission-and-governor.md`.

use hya_proto::{ActorEpoch, OperationId, SessionId, ToolCallId, now_millis};
use sqlx::Row;

use crate::resident_claim::fence_actor_claim;
use crate::{ActorClaim, MAX_ADMISSION_INTENT_BYTES, SessionStore, StoreError, decode_session_key};

const MAX_ACTIVE_ADMISSIONS: u32 = 100;
const MAX_NON_ACTIVE_ADMISSIONS: u32 = 156;
const ADMISSION_COUNTS_SQL: &str = "SELECT \
    COUNT(CASE WHEN state IN ('accepted', 'started') THEN 1 END) AS active, \
    COUNT(CASE WHEN state IN ('queued', 'waiting') THEN 1 END) AS non_active, \
    COUNT(CASE WHEN state IN ('queued', 'accepted', 'started', 'waiting') THEN 1 END) AS total \
 FROM admission_journal";

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
    /// Rows in `accepted` + `started` (cap 100).
    pub active: u32,
    /// Rows in `queued` + `waiting` (cap 156).
    pub non_active: u32,
    /// Sum of nonterminal occupancy counts used for diagnostics.
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

/// Bound spawn payload stored all-or-nothing with the journal row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionIntent {
    /// Version of the runtime fingerprint schema.
    pub runtime_fingerprint_version: u32,
    /// Hash of the runtime binding at claim time.
    pub runtime_fingerprint: [u8; 32],
    /// Version of the admission-binding fingerprint schema.
    pub admission_binding_fingerprint_version: u32,
    /// Hash of admission-specific binding material.
    pub admission_binding_fingerprint: [u8; 32],
    /// Opaque spawn intent blob (1..=[`MAX_ADMISSION_INTENT_BYTES`] bytes).
    pub spawn_intent: Vec<u8>,
}

/// An accepted member row paired with the intent needed to launch it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionLaunch {
    /// Journal row after claim/promotion.
    pub record: AdmissionRecord,
    /// Binding/intent material for the engine.
    pub intent: AdmissionIntent,
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

/// Outcome of batch claim APIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionBatchClaimOutcome {
    /// Newly claimed members with intents (accepted launches only).
    Claimed(Vec<AdmissionLaunch>),
    /// Operation already present; no new members claimed.
    Existing,
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

/// Batch finalize result: terminalized members plus any FIFO promotions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionReleaseOutcome {
    /// Members that reached a terminal state in this call.
    pub finalized: Vec<AdmissionFinalizeOutcome>,
    /// Queued rows promoted into active slots after capacity freed.
    pub promoted: Vec<AdmissionLaunch>,
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

    /// Claim a multi-member batch with binding intents (capacity-checked).
    pub async fn claim_admission_batch(
        &self,
        claim: &AdmissionClaim,
        intents: Vec<AdmissionIntent>,
    ) -> Result<AdmissionBatchClaimOutcome, StoreError> {
        self.claim_admission_batch_impl(claim, intents, None).await
    }

    /// Move a parent member to `waiting`, then claim a child batch in one transaction.
    pub async fn suspend_parent_and_claim_admission_batch(
        &self,
        parent_operation_id: OperationId,
        parent_member_ordinal: u32,
        child_claim: &AdmissionClaim,
        child_intents: Vec<AdmissionIntent>,
    ) -> Result<AdmissionBatchClaimOutcome, StoreError> {
        self.claim_admission_batch_impl(
            child_claim,
            child_intents,
            Some((parent_operation_id, parent_member_ordinal)),
        )
        .await
    }

    async fn claim_admission_batch_impl(
        &self,
        claim: &AdmissionClaim,
        intents: Vec<AdmissionIntent>,
        parent: Option<(OperationId, u32)>,
    ) -> Result<AdmissionBatchClaimOutcome, StoreError> {
        let requested = claim.admission_units;
        if requested == 0 {
            return Err(StoreError::AdmissionData(
                "admission units must be greater than zero".to_string(),
            ));
        }
        let requested_len = usize::try_from(requested).map_err(|_| {
            StoreError::AdmissionData("admission request exceeds usize range".to_string())
        })?;
        if intents.len() != requested_len {
            return Err(StoreError::AdmissionData(
                "admission intent count must equal admission units".to_string(),
            ));
        }
        if intents.iter().any(|intent| {
            intent.spawn_intent.is_empty() || intent.spawn_intent.len() > MAX_ADMISSION_INTENT_BYTES
        }) {
            return Err(StoreError::AdmissionData(
                "spawn intent size must be within the admission intent limit".to_string(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(actor_claim) = &claim.actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }

        let existing = admissions_in_transaction(&mut tx, claim.operation_id).await?;
        if !existing.is_empty() {
            let expected_actor = claim.actor_claim.map(|actor| AdmissionActorBinding {
                actor_id: actor.actor_id,
                actor_epoch: actor.epoch,
            });
            let matches_claim = existing.len() == requested_len
                && existing.iter().enumerate().all(|(index, record)| {
                    u32::try_from(index).ok() == Some(record.member_ordinal)
                        && record.batch_size == requested
                        && record.admission_units == 1
                        && record.operation_id == claim.operation_id
                        && record.source_tool_call_id == claim.source_tool_call_id
                        && record.root_session == claim.root_session
                        && record.request_fingerprint == claim.request_fingerprint
                        && record.actor == expected_actor
                });
            if matches_claim {
                let mut payload_matches = true;
                for (member_ordinal, intent) in intents.iter().enumerate() {
                    let member_ordinal = i64::try_from(member_ordinal).map_err(|_| {
                        StoreError::AdmissionData(
                            "admission member ordinal exceeds SQLite INTEGER range".to_string(),
                        )
                    })?;
                    let stored = sqlx::query(
                        "SELECT 1 FROM admission_journal \
                         WHERE operation_id = ? AND member_ordinal = ? \
                           AND runtime_fingerprint_version = ? \
                           AND runtime_fingerprint = ? \
                           AND admission_binding_fingerprint_version = ? \
                           AND admission_binding_fingerprint = ? \
                           AND spawn_intent = ?",
                    )
                    .bind(claim.operation_id.as_uuid().as_bytes().as_slice())
                    .bind(member_ordinal)
                    .bind(i64::from(intent.runtime_fingerprint_version))
                    .bind(intent.runtime_fingerprint.as_slice())
                    .bind(i64::from(intent.admission_binding_fingerprint_version))
                    .bind(intent.admission_binding_fingerprint.as_slice())
                    .bind(intent.spawn_intent.as_slice())
                    .fetch_optional(&mut *tx)
                    .await?;
                    if stored.is_none() {
                        payload_matches = false;
                        break;
                    }
                }
                if payload_matches {
                    tx.commit().await?;
                    return Ok(AdmissionBatchClaimOutcome::Existing);
                }
            }
            tx.rollback().await?;
            return Err(StoreError::OperationIdConflict {
                operation_id: claim.operation_id,
            });
        }

        if let Some((parent_operation_id, parent_member_ordinal)) = parent {
            let parent_row = sqlx::query(
                "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                        member_ordinal, batch_size, state, admission_units, logical_released, \
                        terminal_reason, created_at, updated_at, actor_id, actor_epoch, \
                        runtime_fingerprint_version, runtime_fingerprint, \
                        admission_binding_fingerprint_version, admission_binding_fingerprint, \
                        spawn_intent \
                 FROM admission_journal \
                 WHERE operation_id = ? AND member_ordinal = ?",
            )
            .bind(parent_operation_id.as_uuid().as_bytes().as_slice())
            .bind(i64::from(parent_member_ordinal))
            .fetch_optional(&mut *tx)
            .await?;
            let Some(parent_row) = parent_row else {
                tx.rollback().await?;
                return Err(StoreError::AdmissionNotFound {
                    operation_id: parent_operation_id,
                });
            };
            let parent_bound = parent_row
                .try_get::<Option<i64>, _>("runtime_fingerprint_version")?
                .is_some()
                && parent_row
                    .try_get::<Option<Vec<u8>>, _>("runtime_fingerprint")?
                    .is_some()
                && parent_row
                    .try_get::<Option<i64>, _>("admission_binding_fingerprint_version")?
                    .is_some()
                && parent_row
                    .try_get::<Option<Vec<u8>>, _>("admission_binding_fingerprint")?
                    .is_some()
                && parent_row
                    .try_get::<Option<Vec<u8>>, _>("spawn_intent")?
                    .is_some();
            let parent_record = decode_record(parent_row)?;
            if parent_record.state != AdmissionState::Started {
                tx.rollback().await?;
                return Err(StoreError::AdmissionTransitionConflict {
                    operation_id: parent_operation_id,
                    from: parent_record.state.as_str(),
                    to: AdmissionState::Waiting.as_str(),
                });
            }
            if !parent_bound {
                tx.rollback().await?;
                return Err(StoreError::AdmissionData(
                    "parent admission binding is incomplete".to_string(),
                ));
            }
            if parent_record.root_session != claim.root_session {
                tx.rollback().await?;
                return Err(StoreError::AdmissionData(
                    "parent and child root sessions must match".to_string(),
                ));
            }
            let expected_actor = claim.actor_claim.map(|actor| AdmissionActorBinding {
                actor_id: actor.actor_id,
                actor_epoch: actor.epoch,
            });
            if parent_record.actor != expected_actor {
                tx.rollback().await?;
                return Err(StoreError::AdmissionData(
                    "parent and child actor bindings must match".to_string(),
                ));
            }
        }

        let source_operation = sqlx::query(
            "SELECT operation_id FROM admission_journal \
             WHERE source_tool_call_id = ? \
             LIMIT 1",
        )
        .bind(claim.source_tool_call_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = source_operation {
            let operation_id: Vec<u8> = row.try_get("operation_id")?;
            let operation_id = uuid::Uuid::from_slice(&operation_id)
                .map_err(|error| {
                    StoreError::AdmissionData(format!("invalid operation id: {error}"))
                })
                .map(OperationId::from_storage_uuid)?;
            if operation_id != claim.operation_id {
                tx.rollback().await?;
                return Err(StoreError::OperationIdConflict {
                    operation_id: claim.operation_id,
                });
            }
        }

        let counts = decode_admission_counts(
            &sqlx::query(ADMISSION_COUNTS_SQL)
                .fetch_one(&mut *tx)
                .await?,
        )?;
        let max_total = MAX_ACTIVE_ADMISSIONS
            .checked_add(MAX_NON_ACTIVE_ADMISSIONS)
            .ok_or_else(|| {
                StoreError::AdmissionData("admission capacity exceeds u32 range".to_string())
            })?;
        let current_total = counts
            .active
            .checked_add(counts.non_active)
            .ok_or_else(|| {
                StoreError::AdmissionData("admission counts exceed u32 range".to_string())
            })?;
        if counts.active > MAX_ACTIVE_ADMISSIONS
            || counts.non_active > MAX_NON_ACTIVE_ADMISSIONS
            || current_total > max_total
        {
            tx.rollback().await?;
            return Err(StoreError::AdmissionData(
                "durable admission counts exceed fixed capacity".to_string(),
            ));
        }

        let (effective_active, effective_non_active) = if parent.is_some() {
            let active = counts.active.checked_sub(1).ok_or_else(|| {
                StoreError::AdmissionData("active admission count underflow".to_string())
            })?;
            let non_active = counts.non_active.checked_add(1).ok_or_else(|| {
                StoreError::AdmissionData(
                    "non-active admission count exceeds u32 range".to_string(),
                )
            })?;
            (active, non_active)
        } else {
            (counts.active, counts.non_active)
        };
        let effective_total = effective_active
            .checked_add(effective_non_active)
            .ok_or_else(|| {
                StoreError::AdmissionData("admission count exceeds u32 range".to_string())
            })?;
        if effective_active > MAX_ACTIVE_ADMISSIONS
            || effective_non_active > MAX_NON_ACTIVE_ADMISSIONS
            || effective_total > max_total
        {
            tx.rollback().await?;
            return Err(StoreError::AdmissionCapacityExceeded {
                active: counts.active,
                non_active: counts.non_active,
                requested,
            });
        }

        let available_active = MAX_ACTIVE_ADMISSIONS - effective_active;
        let accepted = requested.min(available_active);
        let queued = requested.checked_sub(accepted).ok_or_else(|| {
            StoreError::AdmissionData("admission request arithmetic overflow".to_string())
        })?;
        let final_active = effective_active.checked_add(accepted).ok_or_else(|| {
            StoreError::AdmissionData("active admission count exceeds u32 range".to_string())
        })?;
        let final_non_active = effective_non_active.checked_add(queued).ok_or_else(|| {
            StoreError::AdmissionData("non-active admission count exceeds u32 range".to_string())
        })?;
        let final_total = final_active.checked_add(final_non_active).ok_or_else(|| {
            StoreError::AdmissionData("admission count exceeds u32 range".to_string())
        })?;
        if final_active > MAX_ACTIVE_ADMISSIONS
            || final_non_active > MAX_NON_ACTIVE_ADMISSIONS
            || final_total > max_total
        {
            tx.rollback().await?;
            return Err(StoreError::AdmissionCapacityExceeded {
                active: counts.active,
                non_active: counts.non_active,
                requested,
            });
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
        let max_admission_sequence: Option<i64> =
            sqlx::query("SELECT MAX(admission_sequence) AS max_sequence FROM admission_journal")
                .fetch_one(&mut *tx)
                .await?
                .try_get("max_sequence")?;
        let admission_sequence_start =
            next_sequence_start(max_admission_sequence, requested, "admission sequence")?;
        let now = now_millis();
        if let Some((parent_operation_id, parent_member_ordinal)) = parent {
            let parent_updated = sqlx::query(
                "UPDATE admission_journal SET state = 'waiting', updated_at = ? \
                 WHERE operation_id = ? AND member_ordinal = ? AND state = 'started' \
                   AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
                 RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                           member_ordinal, batch_size, state, admission_units, logical_released, \
                           terminal_reason, created_at, updated_at, actor_id, actor_epoch",
            )
            .bind(now)
            .bind(parent_operation_id.as_uuid().as_bytes().as_slice())
            .bind(i64::from(parent_member_ordinal))
            .bind(actor_id.clone())
            .bind(actor_id.clone())
            .bind(actor_epoch)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(parent_updated) = parent_updated else {
                tx.rollback().await?;
                return Err(StoreError::AdmissionTransitionConflict {
                    operation_id: parent_operation_id,
                    from: AdmissionState::Started.as_str(),
                    to: AdmissionState::Waiting.as_str(),
                });
            };
            decode_record(parent_updated)?;
        }
        for member_ordinal in 0..requested {
            let state = if member_ordinal < accepted {
                "accepted"
            } else {
                "queued"
            };
            let intent = &intents[usize::try_from(member_ordinal).map_err(|_| {
                StoreError::AdmissionData(
                    "admission member ordinal exceeds usize range".to_string(),
                )
            })?];
            sqlx::query(
                "INSERT INTO admission_journal \
                 (operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                  state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                  actor_id, actor_epoch, member_ordinal, batch_size, \
                  runtime_fingerprint_version, runtime_fingerprint, \
                  admission_binding_fingerprint_version, admission_binding_fingerprint, spawn_intent, \
                  admission_sequence) \
                 VALUES (?, ?, ?, ?, ?, 1, 0, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(claim.operation_id.as_uuid().as_bytes().as_slice())
            .bind(claim.source_tool_call_id.as_uuid().as_bytes().as_slice())
            .bind(claim.root_session.storage_key())
            .bind(claim.request_fingerprint.as_slice())
            .bind(state)
            .bind(now)
            .bind(now)
            .bind(actor_id.clone())
            .bind(actor_epoch)
            .bind(i64::from(member_ordinal))
            .bind(i64::from(requested))
            .bind(i64::from(intent.runtime_fingerprint_version))
            .bind(intent.runtime_fingerprint.as_slice())
            .bind(i64::from(intent.admission_binding_fingerprint_version))
            .bind(intent.admission_binding_fingerprint.as_slice())
            .bind(intent.spawn_intent.as_slice())
            .bind(admission_sequence_start + i64::from(member_ordinal))
            .execute(&mut *tx)
            .await?;
        }

        let records = admissions_in_transaction(&mut tx, claim.operation_id).await?;
        tx.commit().await?;
        let launches = records
            .into_iter()
            .zip(intents)
            .filter_map(|(record, intent)| {
                (record.state == AdmissionState::Accepted)
                    .then_some(AdmissionLaunch { record, intent })
            })
            .collect();
        Ok(AdmissionBatchClaimOutcome::Claimed(launches))
    }

    /// Transition a `waiting` member to `queued` for later FIFO promotion (idempotent if already queued).
    pub async fn queue_waiting_admission_member(
        &self,
        operation_id: OperationId,
        member_ordinal: u32,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionRecord, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(actor_claim) = actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        let actor_id = actor_claim.map(|claim| claim.actor_id.storage_key());
        let actor_epoch = actor_claim
            .map(|claim| i64::try_from(claim.epoch.get()))
            .transpose()
            .map_err(|_| {
                StoreError::AdmissionData("actor epoch exceeds SQLite INTEGER range".to_string())
            })?;
        let row = sqlx::query(
            "UPDATE admission_journal SET state = 'queued', updated_at = ? \
             WHERE operation_id = ? AND member_ordinal = ? AND state = 'waiting' \
               AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
        )
        .bind(now_millis())
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(i64::from(member_ordinal))
        .bind(actor_id.clone())
        .bind(actor_id.clone())
        .bind(actor_epoch)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = row {
            let record = decode_record(row)?;
            tx.commit().await?;
            return Ok(record);
        }

        let record = sqlx::query(
            "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    member_ordinal, batch_size, state, admission_units, logical_released, \
                    terminal_reason, created_at, updated_at, actor_id, actor_epoch \
             FROM admission_journal \
             WHERE operation_id = ? AND member_ordinal = ?",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(i64::from(member_ordinal))
        .fetch_optional(&mut *tx)
        .await?
        .map(decode_record)
        .transpose()?
        .ok_or(StoreError::AdmissionNotFound { operation_id })?;
        let expected_actor = actor_claim.map(|claim| AdmissionActorBinding {
            actor_id: claim.actor_id,
            actor_epoch: claim.epoch,
        });
        if record.actor != expected_actor {
            tx.rollback().await?;
            return Err(StoreError::AdmissionData(
                "admission actor binding does not match".to_string(),
            ));
        }
        if record.state == AdmissionState::Queued {
            tx.commit().await?;
            return Ok(record);
        }
        let error = StoreError::AdmissionTransitionConflict {
            operation_id,
            from: record.state.as_str(),
            to: AdmissionState::Queued.as_str(),
        };
        tx.rollback().await?;
        Err(error)
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

    /// Count active (`accepted`+`started`) and non-active (`queued`+`waiting`) rows.
    pub async fn admission_counts(&self) -> Result<AdmissionCounts, StoreError> {
        let row = sqlx::query(ADMISSION_COUNTS_SQL)
            .fetch_one(&self.pool)
            .await?;
        decode_admission_counts(&row)
    }

    /// FIFO-promote up to `limit` queued rows into active slots using fairness indexes.
    pub async fn promote_queued_admissions(
        &self,
        limit: u32,
    ) -> Result<Vec<AdmissionLaunch>, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let launches =
            Self::promote_queued_admissions_in_transaction(&mut tx, limit, now_millis()).await?;
        tx.commit().await?;
        Ok(launches)
    }

    async fn promote_queued_admissions_in_transaction(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        limit: u32,
        now: i64,
    ) -> Result<Vec<AdmissionLaunch>, StoreError> {
        let counts = decode_admission_counts(
            &sqlx::query(ADMISSION_COUNTS_SQL)
                .fetch_one(&mut **tx)
                .await?,
        )?;
        let max_total = MAX_ACTIVE_ADMISSIONS
            .checked_add(MAX_NON_ACTIVE_ADMISSIONS)
            .ok_or_else(|| {
                StoreError::AdmissionData("admission capacity exceeds u32 range".to_string())
            })?;
        let current_total = counts
            .active
            .checked_add(counts.non_active)
            .ok_or_else(|| {
                StoreError::AdmissionData("admission counts exceed u32 range".to_string())
            })?;
        if counts.active > MAX_ACTIVE_ADMISSIONS
            || counts.non_active > MAX_NON_ACTIVE_ADMISSIONS
            || current_total > max_total
        {
            return Err(StoreError::AdmissionData(
                "durable admission counts exceed fixed capacity".to_string(),
            ));
        }

        let promotion_limit = limit.min(MAX_ACTIVE_ADMISSIONS - counts.active);
        if promotion_limit == 0 {
            return Ok(Vec::new());
        }
        let mut launches = Vec::new();
        for _ in 0..promotion_limit {
            let Some(row) = sqlx::query(
                "SELECT candidate.operation_id, candidate.member_ordinal \
                 FROM admission_journal AS candidate \
                 WHERE candidate.state = 'queued' \
                   AND candidate.admission_sequence IS NOT NULL \
                   AND candidate.runtime_fingerprint_version IS NOT NULL \
                   AND candidate.runtime_fingerprint IS NOT NULL \
                   AND candidate.admission_binding_fingerprint_version IS NOT NULL \
                   AND candidate.admission_binding_fingerprint IS NOT NULL \
                   AND candidate.spawn_intent IS NOT NULL \
                 ORDER BY CASE WHEN ( \
                     SELECT MAX(history.promotion_sequence) \
                     FROM admission_journal AS history \
                     WHERE history.root_session_id = candidate.root_session_id \
                 ) IS NULL THEN 0 ELSE 1 END, \
                 ( \
                     SELECT MAX(history.promotion_sequence) \
                     FROM admission_journal AS history \
                     WHERE history.root_session_id = candidate.root_session_id \
                 ), \
                 candidate.admission_sequence, candidate.root_session_id, \
                 candidate.operation_id, candidate.member_ordinal \
                 LIMIT 1",
            )
            .fetch_optional(&mut **tx)
            .await?
            else {
                break;
            };
            let operation_id: Vec<u8> = row.try_get("operation_id")?;
            let member_ordinal: i64 = row.try_get("member_ordinal")?;
            let max_promotion_sequence: Option<i64> = sqlx::query(
                "SELECT MAX(promotion_sequence) AS max_sequence FROM admission_journal",
            )
            .fetch_one(&mut **tx)
            .await?
            .try_get("max_sequence")?;
            let promotion_sequence =
                next_sequence_start(max_promotion_sequence, 1, "promotion sequence")?;
            let updated = sqlx::query(
                "UPDATE admission_journal SET state = 'accepted', promotion_sequence = ?, \
                         updated_at = ? \
                 WHERE operation_id = ? AND member_ordinal = ? AND state = 'queued' \
                 RETURNING operation_id, source_tool_call_id, root_session_id, \
                           request_fingerprint, member_ordinal, batch_size, state, \
                           admission_units, logical_released, terminal_reason, created_at, \
                           updated_at, actor_id, actor_epoch, runtime_fingerprint_version, \
                           runtime_fingerprint, admission_binding_fingerprint_version, \
                           admission_binding_fingerprint, spawn_intent",
            )
            .bind(promotion_sequence)
            .bind(now)
            .bind(operation_id)
            .bind(member_ordinal)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(updated) = updated else {
                return Err(StoreError::AdmissionData(
                    "queued admission changed during promotion".to_string(),
                ));
            };
            launches.push(decode_admission_launch(updated)?);
        }

        Ok(launches)
    }

    /// Move a single-member claim from `accepted` to `started`.
    pub async fn start_admission(
        &self,
        operation_id: OperationId,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionStartOutcome, StoreError> {
        self.start_admission_member_impl(operation_id, 0, actor_claim, true)
            .await
    }

    /// Move one batch member from `accepted` to `started` by ordinal.
    pub async fn start_admission_member(
        &self,
        operation_id: OperationId,
        member_ordinal: u32,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionStartOutcome, StoreError> {
        self.start_admission_member_impl(operation_id, member_ordinal, actor_claim, false)
            .await
    }

    async fn start_admission_member_impl(
        &self,
        operation_id: OperationId,
        member_ordinal: u32,
        actor_claim: Option<&ActorClaim>,
        single_row_only: bool,
    ) -> Result<AdmissionStartOutcome, StoreError> {
        let single_row_filter = if single_row_only { 1_i64 } else { 0_i64 };
        let mut tx = self.pool.begin().await?;
        if let Some(actor_claim) = actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        let row = sqlx::query(
            "UPDATE admission_journal SET state = 'started', updated_at = ? \
             WHERE operation_id = ? AND member_ordinal = ? \
               AND (? = 0 OR batch_size = 1) \
               AND state = 'accepted' \
               AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       member_ordinal, batch_size, state, admission_units, logical_released, \
                       terminal_reason, created_at, updated_at, actor_id, actor_epoch",
        )
        .bind(now_millis())
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(i64::from(member_ordinal))
        .bind(single_row_filter)
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
             WHERE operation_id = ? AND member_ordinal = ? \
               AND (? = 0 OR batch_size = 1)",
        )
        .bind(operation_id.as_uuid().as_bytes().as_slice())
        .bind(i64::from(member_ordinal))
        .bind(single_row_filter)
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

    /// Terminalize multiple members and promote queued admissions into freed active slots.
    pub async fn finalize_admission_members(
        &self,
        members: &[(OperationId, u32)],
        terminal: AdmissionTerminal,
        reason: &str,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<AdmissionReleaseOutcome, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(actor_claim) = actor_claim {
            fence_actor_claim(&mut tx, actor_claim).await?;
        }
        if members.is_empty() {
            tx.commit().await?;
            return Ok(AdmissionReleaseOutcome {
                finalized: Vec::new(),
                promoted: Vec::new(),
            });
        }

        let target = terminal.state();
        let expected_actor = actor_claim.map(|claim| AdmissionActorBinding {
            actor_id: claim.actor_id,
            actor_epoch: claim.epoch,
        });
        let actor_id = actor_claim.map(|claim| claim.actor_id.storage_key());
        let actor_epoch = actor_claim
            .map(|claim| i64::try_from(claim.epoch.get()))
            .transpose()
            .map_err(|_| {
                StoreError::AdmissionData("actor epoch exceeds SQLite INTEGER range".to_string())
            })?;
        let now = now_millis();
        let mut finalized = Vec::with_capacity(members.len());
        let mut released_slots = 0_u32;

        for &(operation_id, member_ordinal) in members {
            let record = sqlx::query(
                "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                        member_ordinal, batch_size, state, admission_units, logical_released, \
                        terminal_reason, created_at, updated_at, actor_id, actor_epoch \
                 FROM admission_journal \
                 WHERE operation_id = ? AND member_ordinal = ?",
            )
            .bind(operation_id.as_uuid().as_bytes().as_slice())
            .bind(i64::from(member_ordinal))
            .fetch_optional(&mut *tx)
            .await?
            .map(decode_record)
            .transpose()?
            .ok_or(StoreError::AdmissionNotFound { operation_id })?;

            if record.actor != expected_actor {
                return Err(StoreError::AdmissionData(
                    "admission actor binding does not match".to_string(),
                ));
            }
            if record.state == target {
                finalized.push(AdmissionFinalizeOutcome {
                    record,
                    release_required: false,
                });
                continue;
            }

            let allowed = match target {
                AdmissionState::Completed => record.state == AdmissionState::Started,
                AdmissionState::Cancelled | AdmissionState::Aborted => matches!(
                    record.state,
                    AdmissionState::Queued
                        | AdmissionState::Accepted
                        | AdmissionState::Started
                        | AdmissionState::Waiting
                ),
                AdmissionState::Queued
                | AdmissionState::Accepted
                | AdmissionState::Started
                | AdmissionState::Waiting => false,
            };
            if !allowed {
                return Err(StoreError::AdmissionTransitionConflict {
                    operation_id,
                    from: record.state.as_str(),
                    to: target.as_str(),
                });
            }

            let release_required = record.state == AdmissionState::Started;
            let active_release = matches!(
                record.state,
                AdmissionState::Accepted | AdmissionState::Started
            );
            let updated = sqlx::query(
                "UPDATE admission_journal \
                 SET state = ?, \
                     logical_released = CASE WHEN state = 'started' THEN 1 ELSE 0 END, \
                     terminal_reason = ?, updated_at = ? \
                 WHERE operation_id = ? AND member_ordinal = ? AND state = ? \
                   AND ((? IS NULL AND actor_id IS NULL) OR (actor_id = ? AND actor_epoch = ?)) \
                 RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                           member_ordinal, batch_size, state, admission_units, logical_released, \
                           terminal_reason, created_at, updated_at, actor_id, actor_epoch",
            )
            .bind(target.as_str())
            .bind(reason)
            .bind(now)
            .bind(operation_id.as_uuid().as_bytes().as_slice())
            .bind(i64::from(member_ordinal))
            .bind(record.state.as_str())
            .bind(actor_id.clone())
            .bind(actor_id.clone())
            .bind(actor_epoch)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(updated) = updated else {
                return Err(StoreError::AdmissionData(
                    "admission changed during finalization".to_string(),
                ));
            };
            let finalized_record = decode_record(updated)?;
            if active_release {
                released_slots = released_slots.checked_add(1).ok_or_else(|| {
                    StoreError::AdmissionData(
                        "released admission slots exceed u32 range".to_string(),
                    )
                })?;
            }
            finalized.push(AdmissionFinalizeOutcome {
                record: finalized_record,
                release_required,
            });
        }

        let promoted =
            Self::promote_queued_admissions_in_transaction(&mut tx, released_slots, now).await?;
        tx.commit().await?;
        Ok(AdmissionReleaseOutcome {
            finalized,
            promoted,
        })
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

fn next_sequence_start(
    max_sequence: Option<i64>,
    count: u32,
    name: &str,
) -> Result<i64, StoreError> {
    let first = max_sequence
        .unwrap_or(0)
        .checked_add(1)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| StoreError::AdmissionData(format!("{name} exceeds SQLite INTEGER range")))?;
    first
        .checked_add(i64::from(count).checked_sub(1).ok_or_else(|| {
            StoreError::AdmissionData(format!("{name} exceeds SQLite INTEGER range"))
        })?)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| StoreError::AdmissionData(format!("{name} exceeds SQLite INTEGER range")))?;
    Ok(first)
}

fn decode_admission_count(value: i64, name: &str) -> Result<u32, StoreError> {
    u32::try_from(value)
        .map_err(|_| StoreError::AdmissionData(format!("admission {name} count exceeds u32 range")))
}

fn decode_admission_counts(row: &sqlx::sqlite::SqliteRow) -> Result<AdmissionCounts, StoreError> {
    Ok(AdmissionCounts {
        active: decode_admission_count(row.try_get("active")?, "active")?,
        non_active: decode_admission_count(row.try_get("non_active")?, "non_active")?,
        total: decode_admission_count(row.try_get("total")?, "total")?,
    })
}

fn decode_admission_launch(row: sqlx::sqlite::SqliteRow) -> Result<AdmissionLaunch, StoreError> {
    let runtime_fingerprint_version: i64 = row
        .try_get::<Option<i64>, _>("runtime_fingerprint_version")?
        .ok_or_else(|| {
            StoreError::AdmissionData("runtime fingerprint version is missing".to_string())
        })?;
    let runtime_fingerprint_version = u32::try_from(runtime_fingerprint_version).map_err(|_| {
        StoreError::AdmissionData("runtime fingerprint version exceeds u32 range".to_string())
    })?;
    let runtime_fingerprint: [u8; 32] = row
        .try_get::<Option<Vec<u8>>, _>("runtime_fingerprint")?
        .ok_or_else(|| StoreError::AdmissionData("runtime fingerprint is missing".to_string()))?
        .try_into()
        .map_err(|_| {
            StoreError::AdmissionData("runtime fingerprint must contain 32 bytes".to_string())
        })?;
    let admission_binding_fingerprint_version: i64 = row
        .try_get::<Option<i64>, _>("admission_binding_fingerprint_version")?
        .ok_or_else(|| {
            StoreError::AdmissionData(
                "admission binding fingerprint version is missing".to_string(),
            )
        })?;
    let admission_binding_fingerprint_version =
        u32::try_from(admission_binding_fingerprint_version).map_err(|_| {
            StoreError::AdmissionData(
                "admission binding fingerprint version exceeds u32 range".to_string(),
            )
        })?;
    let admission_binding_fingerprint: [u8; 32] = row
        .try_get::<Option<Vec<u8>>, _>("admission_binding_fingerprint")?
        .ok_or_else(|| {
            StoreError::AdmissionData("admission binding fingerprint is missing".to_string())
        })?
        .try_into()
        .map_err(|_| {
            StoreError::AdmissionData(
                "admission binding fingerprint must contain 32 bytes".to_string(),
            )
        })?;
    let spawn_intent = row
        .try_get::<Option<Vec<u8>>, _>("spawn_intent")?
        .ok_or_else(|| StoreError::AdmissionData("spawn intent is missing".to_string()))?;
    if spawn_intent.is_empty() || spawn_intent.len() > MAX_ADMISSION_INTENT_BYTES {
        return Err(StoreError::AdmissionData(
            "spawn intent size must be within the admission intent limit".to_string(),
        ));
    }

    Ok(AdmissionLaunch {
        record: decode_record(row)?,
        intent: AdmissionIntent {
            runtime_fingerprint_version,
            runtime_fingerprint,
            admission_binding_fingerprint_version,
            admission_binding_fingerprint,
            spawn_intent,
        },
    })
}

async fn admissions_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_id: OperationId,
) -> Result<Vec<AdmissionRecord>, StoreError> {
    let mut records = sqlx::query(
        "SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                member_ordinal, batch_size, state, admission_units, logical_released, \
                terminal_reason, created_at, updated_at, actor_id, actor_epoch \
         FROM admission_journal \
         WHERE operation_id = ? \
         ORDER BY member_ordinal",
    )
    .bind(operation_id.as_uuid().as_bytes().as_slice())
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(decode_record)
    .collect::<Result<Vec<_>, _>>()?;
    records.sort_by_key(|record| record.member_ordinal);
    Ok(records)
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
